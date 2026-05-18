# Clarion v0.1 — Detailed Design Reference

**Status**: Baselined for v0.1 implementation (post-ADR sprint 2026-04-18) — Layer 3 of the Clarion v0.1 docset, implementation-level reference. Canonical home of the ADR backlog, SQL schema, Rust struct definitions, full YAML config example, exact rule-ID catalogues, wire-format mapping tables, and cross-tool prerequisite lists.
**Baseline**: 2026-04-17 · **Last updated**: 2026-04-18
**Primary author**: qacona@gmail.com (with Claude)
**First customer target**: `/home/john/elspeth` (~425k LOC Python)
**Revision**: 5 (final pre-restructure single-file revision; see Appendix D for full history). Post-restructure the three layered docs track changes via git log plus dated edits in this preamble; there is no unified "Rev 6."

**Companion documents**:
- [requirements.md](./requirements.md) — requirements (the *what*; REQ-* / NFR-* / CON-* / NG-*)
- [system-design.md](./system-design.md) — system design (the *how*, mid-level; architecture, mechanisms, diagrams)
- [reviews/pre-restructure/design-review.md](./reviews/pre-restructure/design-review.md) — design review that drove revisions 2-4
- [reviews/pre-restructure/integration-recon.md](./reviews/pre-restructure/integration-recon.md) — integration reality check against Filigree / Wardline

---

## Preamble

### What this document is

This is Clarion v0.1's **detailed design reference** — implementation-level depth. Everything here is concrete: Rust struct definitions, the full SQL schema, the complete `clarion.yaml` example, the exhaustive Phase-7 rule catalogue with thresholds, the complete MCP tool list, severity mapping tables, the `scan_run_id` lifecycle, the dedup collision policy, SARIF property-bag translation rules, full Filigree/Wardline prerequisite lists (with specific file references and ADR citations), the testing strategy, the ADR backlog with full priorities, and the appendices (future direction, glossary, Rust stack).

### What moved up

Material covering *what Clarion does* (capabilities, quality attributes, non-goals) is in the **requirements** layer. Material covering *how Clarion is structured at architectural level* (component topology, data-model concepts, integration patterns, mid-level mechanisms, architectural trade-offs) is in the **system-design** layer. The abstract, design principles, process/UX topology, conceptual data model, core/plugin split narrative, integration posture, threat model, suite-bootstrap architecture, and explicit-deferrals / non-goals all live in those higher layers now. Nothing has been lost — moved, not deleted.

### When to read what

- **Starting fresh on Clarion?** Read [../../suite/briefing.md](../../suite/briefing.md) + [../../suite/loom.md](../../suite/loom.md) first, then [requirements.md](./requirements.md) + [system-design.md](./system-design.md) in that order. This document fills in detail only when you're ready to implement or debug a specific subsystem.
- **Answering "what does Clarion guarantee?"** Requirements.
- **Answering "how is it structured?"** System design.
- **Answering "what's the exact `busy_timeout`? What's the full Phase-7 rule ID list? Which ADR says what?"** Here.

### How this document is organised

Sections 1-11 are implementation detail by subsystem, mirroring the natural order of "what the implementer is building next." Appendices A-D hold the future-direction note, glossary, Rust stack recommendations, and revision history. Section numbering is clean (not gapped) to match the document's reduced scope.

---

## 1. Plugin Implementation Detail

### Plugin packaging (v0.1)

Each plugin is a separately-installable Python package that provides an executable entry point matching the Clarion plugin protocol. Core finds plugins via `~/.config/clarion/plugins.toml` or via the project's `clarion.yaml`.

**Isolation**: recommended install path is `pipx install clarion-plugin-python`. Plain `pip install clarion-plugin-python` into an unknown active environment risks dependency conflicts with the analyzed project. `plugins.toml` records the plugin's `executable` path (typically a pipx-managed shim) and declared `python_version`; the core refuses to launch a plugin whose `python_version` mismatches the configured expectation.

```toml
# ~/.config/clarion/plugins.toml
[plugins.python]
executable = "~/.local/pipx/venvs/clarion-plugin-python/bin/clarion-plugin-python"
python_version = ">=3.11,<3.14"
version = "1.0"
```

### Plugin manifest shape

Plugin ships a manifest read by the core on `initialized`. Abridged example (Python plugin):

```yaml
name: python
version: 1.0
language_id: python

kinds:
  function:      { leaf: true, searchable: true, has_callers: true }
  class:         { leaf: true, searchable: true, has_members: true }
  protocol:      { leaf: true, searchable: true, subtype_of: class }
  enum:          { leaf: true, searchable: true, subtype_of: class }
  typed_dict:    { leaf: true, searchable: true, subtype_of: class }
  global:        { leaf: true, searchable: true }
  decorator:     { leaf: true, searchable: true, subtype_of: function }
  type_alias:    { leaf: true, searchable: true, subtype_of: global }
  module:        { leaf: false, searchable: true, contains: [function, class, global, ...] }
  package:       { leaf: false, searchable: true, contains: [module, package] }

edges:
  contains:      { cardinality: one_to_many }
  calls:         { cardinality: many_to_many, weighted: true }
  imports:       { cardinality: many_to_many }
  inherits_from: { cardinality: many_to_many }
  implements:    { cardinality: many_to_many }
  decorated_by:  { cardinality: many_to_many }
  references:    { cardinality: many_to_many }

tags:
  - entry_point
  - http_route
  - cli_command
  - data_model
  - config_loader
  - test_function
  - test_class
  - fixture
  - deprecated
  - has_wardline_annotation
  - is_dependency_hub

capabilities:
  parse_files: true
  entity_extraction: true
  edge_extraction:
    contains:      { supported: true, confidence_basis: ast_match }
    imports:       { supported: true, confidence_basis: ast_match }
    calls:         { supported: true, confidence_basis: ast_match }   # reliable for direct same-scope calls;
                                                                      # approximate (name-match) for method calls;
                                                                      # no dynamic dispatch (see "Call graph precision"
                                                                      # under Python plugin specifics below)
    inherits_from: { supported: true, confidence_basis: ast_match }
    decorated_by:  { supported: true, confidence_basis: ast_match }
  annotation_detection:
    # All 17 Wardline groups are decorator-based (verified against
    # wardline.core.registry.REGISTRY: 42 canonical names + 1 legacy alias).
    # Supplementary manifest declarations exist only for Groups 1 and 17
    # via overlay boundaries[]; those are ingested in Phase 7 and augment
    # (don't replace) decorator detection.
    wardline_groups:           [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17]
    wardline_overlay_groups:   [1, 17]      # boundaries[] in overlay.schema.json — additive
    wardline_registry_import:  "wardline.core.registry:REGISTRY"  # direct Python import; see §9
  structural_findings: true
  factual_findings: true

rules:  # abridged example; exact emitted catalogue lives in §5 and the authored ADR set
  - { id: CLA-PY-STRUCTURE-001, description: "Circular import detected", severity: WARN }
  - { id: CLA-PY-STRUCTURE-002, description: "Module-level side effect at import time", severity: INFO }
  - { id: CLA-FACT-TODO, description: "TODO marker in comment", severity: INFO, kind: fact }
  - { id: CLA-FACT-ENTRYPOINT, description: "CLI/HTTP entry point detected", severity: INFO, kind: fact }
  # Additional plugin-declared rules are omitted here for readability; see §5 for
  # the concrete emitted findings and ADR-017 / ADR-022 for naming rules.

prompt_templates:
  function:   templates/summarise_function.md
  class:      templates/summarise_class.md
  protocol:   templates/summarise_protocol.md
  module:     templates/summarise_module.md
  global:     templates/summarise_global.md
```

### Error handling posture

- Plugin crash during batch → core records which files completed (by completed `file_analyzed` messages); unfinished files logged as `CLA-INFRA-PLUGIN-CRASH`; partial run manifest written. Crash-loop circuit breaker (>3 crashes in 60s) halts the plugin permanently for the run.
- Plugin timeout on one file (default 30s, configurable per-plugin) → emit `CLA-INFRA-PLUGIN-TIMEOUT`; skip; continue.
- Plugin malformed JSON or framing error → core logs, skips the message; continues. Never crashes on plugin misbehaviour.
- No silent fallbacks. Every skip/error produces a finding.
- **stdout hygiene**: the plugin protocol reserves stdout for framed JSON-RPC. Plugin authors must redirect logging to stderr; a stray `print()` breaks the stream. This is documented as a plugin-author requirement and enforced by the reference Python client.

### Python plugin specifics — decorator-detection table

| Case | Pattern | Policy |
|---|---|---|
| Direct named | `@validates_shape` | Exact name match against `wardline.core.registry.REGISTRY` (the 42 canonical names). Imported directly; see §9 prerequisite on `REGISTRY_VERSION` pinning. |
| Factory | `@validates_shape("User")` | Match on the callee name; arguments captured into `decorated_by.properties.args` for Wardline to consume |
| Stacked | `@a\n@b\ndef f():` | Emits edges in source order; ordering preserved in `decorated_by.properties.stack_index` (Wardline semantics depend on this) |
| Function decorator | `@register\ndef f():` | Fully supported — matches Wardline's own scanner coverage |
| Class decorator | `@register\nclass C:` | Recorded as decorator edges. **Clarion-side augmentation**: Wardline's own scanner does not visit class-level decoration (`scanner/discovery.py:_walk_functions` only walks FunctionDef/AsyncFunctionDef). Clarion findings derived from class-decoration carry `confidence_basis: "clarion_augmentation"` to distinguish them from Wardline-authoritative claims. |
| Aliased | `validates = validates_shape` | Detected when `validates_shape` is imported and aliased within a module; edge annotated `via_alias: true` |
| Dotted single-level | `@wardline.validates_shape` | Edge records `(module, name)` pair; matched against `REGISTRY` |
| Dotted call | `@app.route("/health")` | Edge records full call chain (`app.route`); tag lookup resolves `app.route` via Wardline's annotation descriptor (see Appendix A — deferred to v0.2) |
| Legacy alias | `@tier_transition` | Resolved via Wardline's `LEGACY_DECORATOR_ALIASES` (`core/registry.py:12-14`); canonical name (`trust_boundary`) recorded in `decorated_by.properties.canonical_name`; `via_legacy_alias: true` |

Cases the plugin does *not* claim to handle (and emits `CLA-PY-ANNOTATION-AMBIGUOUS` for): dynamic decorator selection (`if cond: deco = a else b`), runtime-computed attribute decoration, decorators applied via `__init_subclass__`, arbitrary dotted chains (`a.b.c`), star imports (refused by Wardline itself), metaclass-based decoration, lambdas/subscripts as decorators. These are shared blind spots with Wardline's scanner.

### Python plugin specifics — import resolution

- `sys.path` discovery: via the `python_executable` declared in `plugins.toml` (default: the pipx venv) invoked as `python -m site --user-site` plus project-local `PYTHONPATH`. Virtualenvs in `<project>/.venv/` or `<project>/venv/` detected automatically; user can override via `clarion.yaml:analysis.python.sys_path`.
- Unresolvable imports: emit `CLA-PY-UNRESOLVED-IMPORT` finding (kind: fact, severity: INFO); create a **stub** entity with id `python:unresolved:<module.path>` and an `imports` edge to it. Stubs are reconciled against real entities if the import becomes resolvable in a later run; never promoted silently.
- Re-exports: definition site wins. `__init__.py` re-exports produce an `alias_of` edge from the re-export entity to the defining entity (not a new top-level entity). Entity IDs reference the definition site; consult tools resolve aliases transparently.
- Conditional imports: `TYPE_CHECKING` blocks are extracted as `imports` edges with `type_only: true` property — the edge exists so `goto`/search finds them, but graph algorithms (circular-import detection, coupling hotspots) filter them out. `try/except ImportError` blocks: first branch wins (the one that represents the "normal" path); fallback branches emit `CLA-FACT-CONDITIONAL-IMPORT` findings. `if sys.version_info`: all branches union.

### Python plugin specifics — call graph precision

AST-only analysis produces reliable `calls` edges for direct same-scope calls, approximate (name-match) for method calls, and **no edges** for fully-dynamic dispatch. The manifest-level `edge_extraction.calls.confidence_basis: ast_match` reflects this; consult tools surfacing call data must carry the same qualifier through to agents consuming it.

### Python plugin specifics — serial-or-parallel posture

v0.1 serial over files within one process. Parallelism deferred to v0.2 pending evidence that it beats core-side parallelism (the core already parallelises LLM calls across plugin-emitted batches). A serial plugin is debuggable, deterministic, and avoids cross-thread complexity in Python import-resolution state. If profiling against elspeth shows Phase 1 as a bottleneck, v0.2 adds `analyze_file_batch` RPC with partitioning by top-level package.

### Observe-vs-enforce coupling (Principle 5) — v0.1 reality

The plugin manifest above names Wardline annotation groups directly. **v0.1 reality**: Wardline ships the canonical descriptor as `wardline.core.registry.REGISTRY` (`MappingProxyType` of 42 canonical decorator names with `REGISTRY_VERSION`). Clarion's Python plugin imports it directly at startup — not a new artifact, an existing one used in the right direction. The "hardcoded match" concern is resolved: when Wardline adds an annotation, it lands in `REGISTRY`; Clarion's plugin picks it up on next run. Version skew emits `CLA-INFRA-WARDLINE-REGISTRY-STALE`. See §9 for the direct-import contract.

**v0.2 generalisation**: Wardline adds a `wardline annotations descriptor --format yaml` export for non-Python plugins (Java, Rust) that cannot import Python objects directly. This matters only when Clarion adds non-Python plugins; v0.1 ships Python-only and the direct import suffices.

---

## 2. Data Model — Implementation Shapes

### Entity (Rust)

```rust
struct Entity {
    // Identity
    id: EntityId,                      // "python:class:auth.tokens::TokenManager" — stable across file moves
    plugin_id: String,                 // "python", "java", ...; or "core" for guidance/subsystem/file
    kind: String,                      // plugin-defined: "function" | "class" | "protocol" | "global" | "module" | ...
    name: String,                      // fully-qualified display name
    short_name: String,                // last segment

    // Hierarchy
    parent_id: Option<EntityId>,

    // Source anchor (None for non-source entities like subsystems, guidance)
    // file_path lives on the SourceRange, NOT in the ID.
    source: Option<SourceRange>,       // { file_id, byte_start, byte_end, line_start, line_end }

    // Plugin-defined
    tags: BTreeSet<String>,
    properties: BTreeMap<String, JsonValue>,

    // Core-managed
    content_hash: Blake3Hash,
    summary: Option<Summary>,
    wardline: Option<WardlineMeta>,
    created_at: DateTime,
    updated_at: DateTime,

    // Commit provenance — the SHA at the time of the run that first saw
    // this entity, and the SHA of the most recent run that still saw it.
    // Dirty-tree runs store the underlying SHA (pre-`-dirty` suffix) so
    // the values are always real commits.
    first_seen_commit: Option<GitSha>,
    last_seen_commit: Option<GitSha>,
}
```

### Entity ID scheme

- **Source entities**: `{plugin_id}:{kind}:{canonical_qualified_name}` where `canonical_qualified_name` is the plugin's language-native fully-qualified identifier (for Python: `auth.tokens.TokenManager`, not `src/auth/tokens.py::TokenManager`).
- **Files**: `core:file:{content_addressed_path_hash}@{path}` — content-addressed so rename detection has a handle; `@{path}` suffix preserves human readability in logs.
- **Subsystems**: `core:subsystem:{cluster_hash}` (from sorted member module IDs).
- **Guidance sheets**: `core:guidance:{content_hash_short}`.
- **Unresolvable Python imports** (stub): `python:unresolved:{module.path}` — reconciled to real entities when resolution becomes possible.

### Canonical-name policy (Python)

- **Definition site wins**. `TokenManager` defined in `auth/tokens.py` and re-exported from `auth/__init__.py` has ID `python:class:auth.tokens::TokenManager`. The re-export entity is an `alias_of` edge, not a new top-level entity. Consult tools resolve aliases transparently.
- **`src.` prefix stripped**. Projects using `src/` layout get `src.auth.tokens` → `auth.tokens` canonicalisation; policy configurable via `clarion.yaml:analysis.python.canonical_root` (default: auto-detect from `pyproject.toml` `[tool.setuptools.packages.find]` or `[project.optional-dependencies]`).
- **Test and script modules** keep their on-disk module path (no canonicalisation) because they lack a deterministic install path.

### Stability properties

- File moves that don't change the canonical qualified name → ID unchanged. (Moving `auth/tokens.py` to `auth/security/tokens.py` *does* change `auth.tokens` to `auth.security.tokens`; that's a rename, not a move.)
- Content changes → ID unchanged (`content_hash` separate, used for cache invalidation).
- Symbol renames → ID changes. Rename tracking via `EntityAlias` table is a v0.2 feature; the v0.1 posture handles the 80% case (file move without rename) without explicit alias tracking.
- Same-named classes in different modules are distinguished by their canonical path (`auth.tokens.TokenManager` vs `billing.tokens.TokenManager`).

**Known v0.1 limitation**: symbol rename without file move (e.g., `class TokenManager` → `class JwtTokenManager`) detaches every cross-tool reference. Filigree issues tagged with the old ID become orphans until v0.2's `EntityAlias` ships. Document this prominently in operator-facing release notes; the mitigation for v0.1 users is to record renames as annotated commits and run `clarion analyze --repair-aliases <old_id> <new_id>` (a v0.1 CLI command that inserts a manual alias row).

### Edge (Rust)

```rust
struct Edge {
    from: EntityId,
    to: EntityId,
    kind: String,                      // plugin-defined OR core-reserved
    properties: BTreeMap<String, JsonValue>,
    source: Option<SourceRange>,
}
```

**Edge identity and coexistence policy**: the store admits at most one edge for a
given `(kind, from_id, to_id)` tuple. `properties` carries evidence and
kind-specific metadata (for example ambiguous-call candidates), but it is not
part of edge identity and does not allow multiple same-kind relationships
between the same endpoints to coexist. Re-analysis and repeated plugin evidence
therefore remain idempotent; callers that need several observations for one
relationship must merge them into the single edge's properties shape rather than
emitting duplicate rows.

**Core-reserved edge kinds**: `contains`, `guides`, `emits_finding`, `in_subsystem`. All others are plugin-defined.

### Finding (Rust)

```rust
struct Finding {
    // Identity and provenance
    id: FindingId,
    provenance: Provenance { tool: String, tool_version: String, run_id: String },
    rule_id: String,                     // namespaced: "CLA-PY-STRUCTURE-001" | "PY-WL-001-GOVERNED-DEFAULT" | "COV-001"

    // Claim shape — internal representation. Coerced to Filigree's
    // severity vocabulary {critical,high,medium,low,info} on emit (see §7).
    kind: FindingKind,                   // defect | fact | classification | metric | suggestion
    severity: InternalSeverity,          // INFO | WARN | ERROR | CRITICAL for defects; NONE for facts
    confidence: Option<f32>,             // 0.0..=1.0; None = deterministic
    confidence_basis: Option<String>,    // "ast_match" | "llm_inference" | "heuristic" | "dataflow" | "clarion_augmentation"

    // Subject
    entity_id: EntityId,
    related_entities: Vec<EntityId>,
    message: String,
    evidence: Vec<Evidence>,
    properties: BTreeMap<String, JsonValue>,   // in-store only; does NOT travel to Filigree as-is
                                                // (Filigree's extension slot is `metadata` — see §7)

    // Cross-tool chains
    supports: Vec<FindingId>,
    supported_by: Vec<FindingId>,

    // Triage (inline for v0.1; separable later). Status vocabulary here is
    // Clarion-internal; Filigree's own vocabulary is {open, acknowledged,
    // fixed, false_positive, unseen_in_latest} — mapping documented in §7.
    status: "open" | "acknowledged" | "suppressed" | "promoted_to_issue",
    suppression_reason: Option<String>,
    filigree_issue_id: Option<String>,

    created_at: DateTime,
    updated_at: DateTime,
}

enum FindingKind {
    Defect,           // violation or bug — has severity
    Fact,             // observation of structure/behaviour — severity NONE
    Classification,   // "this entity is of type X" — with confidence
    Metric,           // quantitative measurement
    Suggestion,       // non-enforced recommendation
}
```

### Wire format example (Filigree intake)

```json
{
  "scan_source": "clarion",
  "scan_run_id": "run-2026-04-17-153002",
  "mark_unseen": false,
  "create_observations": false,
  "complete_scan_run": true,
  "findings": [
    {
      "path": "src/auth/tokens.py",
      "rule_id": "CLA-PY-STRUCTURE-001",
      "message": "Circular import detected between auth.tokens and auth.sessions",
      "severity": "medium",
      "line_start": 12,
      "line_end": 12,
      "suggestion": "Move the shared type to a third module",
      "metadata": {
        "kind": "defect",
        "confidence": 0.95,
        "confidence_basis": "ast_match",
        "clarion": {
          "entity_id": "python:class:auth.tokens::TokenManager",
          "related_entities": ["python:class:auth.sessions::SessionStore"],
          "supports": [],
          "supported_by": [],
          "internal_severity": "WARN",
          "internal_status": "open"
        }
      }
    }
  ]
}
```

Key properties of the wire schema (verified against Filigree source — `dashboard_routes/files.py:294-330`, `db_files.py:386-556`):

- **Extension slot is `metadata`** (a dict), **not** `properties`. Any top-level finding key outside the enumerated set — `path`, `rule_id`, `message`, `severity`, `line_start`, `line_end`, `suggestion`, `language`, `metadata` — is **silently dropped**. Clarion's richer fields must nest under `metadata` to survive.
- **Line fields are `line_start` + `line_end`**, not a single `line`.
- **Severity enum on the wire is `{critical, high, medium, low, info}`** — all lowercase. Unknown values are coerced to `"info"` and surfaced in the response's `warnings[]` array. Clarion's internal `{INFO, WARN, ERROR, CRITICAL}` vocabulary is mapped to this wire vocabulary on emit (see §7 mapping table). The internal value is preserved in `metadata.clarion.internal_severity` for round-trip.
- **`scan_run_id` is optional**. If present and `complete_scan_run=true`, Filigree attempts to close the run; unknown IDs log a warning and continue.
- **Dedup key** on Filigree's side is `(file_id, scan_source, rule_id, coalesce(line_start, -1))` (`db_schema.py:156-157`). Re-posts with the same 4-tuple overwrite; see §7 for Clarion's dedup-collision policy when entities move within a file.
- **Clarion must inspect `response.warnings[]`** (not just the count) on every POST to detect silent severity coercion or unknown-key drops.

### Entity Briefing (structured summary)

```rust
struct EntityBriefing {
    purpose: String,                 // 1-2 sentences: what this is and why it exists
    maturity: Maturity,
    maturity_reasoning: Option<String>,

    risks: Vec<RiskItem>,
    patterns: Vec<String>,           // tagged from controlled vocabulary
    antipatterns: Vec<String>,       // tagged from controlled vocabulary

    relationships: KeyRelationships,

    // Epistemic metadata — lets agents consuming the briefing calibrate
    // whether they should spawn exploration that the cache can't answer.
    knowledge_basis: KnowledgeBasis,

    notes: Option<String>,
}

enum KnowledgeBasis {
    StaticOnly,       // everything derives from AST + imports + declared annotations
    RuntimeInformed,  // coverage data, trace data, or execution evidence contributed
                      // — v0.1 sources: none (reserved for v0.2 coverage ingest)
    HumanVerified,    // operator authored guidance matching this entity, OR
                      // operator acknowledged / suppressed a finding on this entity
                      // with a non-empty reason
}

// knowledge_basis promotion rule (applied at Phase 7, v0.1):
//
// Default: StaticOnly.
// Promote to HumanVerified if either:
//   (a) A guidance sheet whose match_rules resolve to this entity was
//       authored or reviewed within the last 90 days (configurable).
//   (b) This entity has a finding with status in {suppressed, acknowledged}
//       and a non-empty suppression_reason (sourced from Filigree).
//
// The promotion is computed at run time from the current guidance/finding
// state, not stored. A guidance sheet expiry naturally reverts the
// basis to StaticOnly on the next run, which is the desired behaviour.

enum Maturity {
    Placeholder, Experimental, Alpha, Beta, Stable, Mature, Deprecated, Dead
}

struct RiskItem {
    tag: String,                     // "concurrency" | "security" | "correctness" | "performance" | "coupling" | "missing-tests" | "data-loss" | ...
    severity: Severity,
    description: String,
    evidence: Option<String>,        // source range / finding ID / filigree issue
}

struct KeyRelationships {
    parents: Vec<RelationshipRef>,
    siblings: Vec<RelationshipRef>,
    children: Vec<RelationshipRef>,
}

struct RelationshipRef {
    entity_id: EntityId,
    name: String,
    kind: String,
    relationship: String,
    why_relevant: String,
}
```

**Detail levels**:

| Level | Fields | Typical token size |
|---|---|---|
| `short` | `purpose` + `maturity` only | ~60 |
| `medium` | + top-3 risks, up to 5 patterns, up to 3 antipatterns, parent + top-3 siblings + top-3 children | ~300 |
| `full` | + maturity_reasoning, all risks (to 10), all patterns, all relationships (to 8 each) | ~900 |
| `exhaustive` | + notes, fully elaborated `why_relevant` | ~1,800 |

**Vocabulary approach** (for patterns, antipatterns, risk tags):
- Core ships base vocabulary.
- Plugins extend with language-specific entries.
- Unknown tags from LLM accepted; logged as `CLA-FACT-VOCABULARY-CANDIDATE`; surfaced via `clarion vocabulary candidates` report for human review.

**Validation**: briefings are generated via Anthropic's structured-output feature; schema-validated on receipt; invalid responses trigger one retry then emit `CLA-INFRA-BRIEFING-INVALID`.

### Guidance sheet entity

```
Entity {
    kind: "guidance",
    name: "JWT token module guidance",
    parent_id: null,
    tags: [],
    properties: {
        content: "<markdown>",
        content_hash: "<blake3>",
        scope_level: "project" | "subsystem" | "package" | "module" | "class" | "function",
        // Composition rank derived from scope_level via the schema's CASE-mapped
        // generated column (see §3 and ADR-024). 1 = project (outermost,
        // lowest precedence), 6 = function (innermost, highest precedence).
        scope: {
            query_types: ["summary", "wardline", "consult"],
            token_budget: 600,
        },
        match_rules: [
            { type: "path", pattern: "src/auth/tokens/**" },
            { type: "tag", value: "integral_writer" },
            { type: "kind", value: "python:protocol" },
            { type: "wardline_group", value: 2 },
            { type: "subsystem", id: "core:subsystem:auth" },
            { type: "entity", id: "python:class:..." }
        ],
        expires: Option<DateTime>,
        // Pinned sheets are preserved across token-budget pressure when other
        // sheets are dropped to fit the budget. Per ADR-024 this is a
        // budget-protection behaviour, not a UI-sort behaviour.
        pinned: bool,
        authored_by: String,
        authored_at: DateTime,
        reviewed_at: DateTime,
        // Provenance: how the sheet came to exist. See ADR-024 for the
        // role-vs-shape rationale; finding.provenance uses the same word for
        // the same role with a struct shape.
        provenance: "manual" | "wardline_derived" | "filigree_promotion",
        provenance_ref: Option<String>,
    },
    source: null,  // SourceRange — guidance entities have no code anchor
}
```

Guidance sheets with explicit entity targets get `guides` edges; pattern-based matches (path, tag, kind, wardline group, subsystem) resolve at query time.

### File entity

```
Entity {
    plugin_id: "core",
    kind: "file",
    name: "src/auth/tokens.py",
    short_name: "tokens.py",
    parent_id: <package entity id>,
    source: { file_id: self, byte_start: 0, byte_end: <size>, line_start: 1, line_end: <lines> },
    tags: ["python"] | [],
    properties: {
        size_bytes: 4321,
        line_count: 187,
        mime_type: "text/x-python",
        git_last_modified: "2026-04-10T...",
        git_last_modified_sha: "abc123...",
        git_churn_count: 47,
        git_authors: ["alice", "bob"],
    },
}
```

### Subsystem entity (core-emitted)

```
Entity {
    plugin_id: "core",
    kind: "subsystem",
    name: "Authentication",
    parent_id: null (or parent subsystem for multi-level clustering),
    properties: {
        cluster_algorithm: "leiden",
        modularity_score: 0.42,
        member_count: 14,
        synthesised_at: DateTime,
    },
}
```

Member modules/packages have `in_subsystem` edges to the subsystem.

### Summary (storage shape)

```rust
struct Summary {
    briefing: EntityBriefing,
    length_class: "short" | "medium" | "full" | "exhaustive",
    model: String,                     // concrete model ID used
    model_tier: String,                // "haiku" | "sonnet" | "opus"
    prompt_template_id: String,        // "python:class:v1"
    guidance_fingerprint: Blake3Hash,
    generated_at: DateTime,
    cost_usd: f64,
    tokens: { input: u32, output: u32 },
}
```

### Wardline metadata

```rust
struct WardlineMeta {
    declared_tier: Option<TierName>,   // "INTEGRAL" | "ASSURED" | "GUARDED" | "EXTERNAL_RAW"
                                        // — Wardline's canonical names, manifest-configurable
    declared_groups: BTreeSet<u32>,    // 1..=17
    declared_boundary_contracts: Vec<String>,
    effective_state: Option<String>,   // computed by wardline enforcer; v0.2 via read API
    provenance: "manifest" | "overlay" | "inferred" | "fingerprint_json",
    annotation_hash: Option<String>,   // from wardline.fingerprint.json; SHA-256 over ast.dump
    wardline_qualname: Option<String>, // Wardline's own qualname (class.method dotted form);
                                        // enables cross-tool join — see "Identity reconciliation" below
}
```

### Identity reconciliation across the suite

Three independent identity schemes exist across Clarion, Wardline, and Wardline's exception register:

| Scheme | Example | Owner | Format |
|---|---|---|---|
| Clarion `EntityId` | `python:class:auth.tokens::TokenManager` | Clarion | `{plugin_id}:{kind}:{canonical_qualified_name}` |
| Wardline `qualname` | `TokenManager.verify` | Wardline `FingerprintEntry` | Nested class/method dotted form (NOT Python `__qualname__`; no `<locals>` suffix) |
| Wardline exception-register `location` | `src/wardline/scanner/engine.py::ScanEngine._scan_file` | `wardline.exceptions.json` | `{file_path}::{qualname}` with double-colon separator |

None of these strings are byte-equal for the same underlying symbol. Clarion v0.1 reconciles them explicitly:

- **Ingest path from `wardline.fingerprint.json`**: for each `FingerprintEntry`, Clarion computes `(file_path, qualname) → EntityId` via Wardline's own `module_file_map` (available at scan time from `ScanContext`). The reverse mapping (`EntityId → wardline_qualname`) is recorded as an entity property so future Wardline-authored findings can be cross-referenced back to Clarion entities.
- **Ingest path from `wardline.exceptions.json`**: the `location` string is parsed (`split("::", 1)` yields `{file_path, qualname}`); same mapping rule applies. Exception entries that can't be resolved (e.g., file moved, symbol renamed) emit `CLA-INFRA-WARDLINE-EXCEPTION-UNRESOLVED` and persist as dangling records with `entity_id: null` so operators can fix the reference.
- **Ingest path from SARIF (`wardline.sarif.baseline.json`)**: SARIF `location.physicalLocation.artifactLocation.uri` is already POSIX-relative (Wardline's `_normalize_artifact_uri` at `sarif.py:233-246`); combined with `location.logicalLocations[].fullyQualifiedName` (when present) or `partialFingerprints` (when Wardline doesn't emit them, with Clarion's own fingerprint) to produce an `EntityId`. Unresolved SARIF results carry `metadata.clarion.unresolved: true` through translation.

Clarion does *not* push a unified identity scheme into Wardline. Wardline remains authoritative for its own qualnames; Clarion maintains a translation layer. This preserves Principle 3 (plugin owns ontology) — Wardline's identity scheme is its own concern; Clarion's concern is producing a reliable join.

The v0.2 **Wardline annotation descriptor** (§9, promoted from earlier deferral) is an opportunity to also standardise qualname emission: if Wardline starts publishing qualnames in a descriptor alongside its REGISTRY, Clarion's reconciliation simplifies from a heuristic map to a direct lookup.

---

## 3. Storage Implementation

### SQLite rationale

- Graph algorithms (Leiden, weighted-components fallback, centrality, path-finding) run in Rust against in-memory projections; the store serves neighbour lookups.
- Consult-mode queries are overwhelmingly one-hop (callers, callees, contains) — indexed range scans.
- WAL mode with a **writer-actor** (single owner of the write connection; see Concurrency below) permits `clarion serve` to keep answering reads while `clarion analyze` is ingesting and while consult-mode cache writes happen.
- JSON1 handles plugin property bags without giving up query ergonomics.
- FTS5 handles text search across names, summaries, content.
- Single-file, mature, debuggable with `sqlite3` / Datasette / VSCode extensions.
- Consistent with the "enterprise at lack of scale" commitment.

Alternatives considered and rejected for v0.1:
- **Kuzu** — native graph, tempting; rejected for v0.1 because immature relative to SQLite and the specific operations we need don't benefit meaningfully from graph-native storage. Repository layer kept thin enough to swap in v0.3+ if profiling demands it.
- **DuckDB** — OLAP-optimised; wrong shape for per-query cursor-chasing.
- **Custom embedded graph** — out of scope.

### Schema (outline)

```sql
-- Entities
CREATE TABLE entities (
    id TEXT PRIMARY KEY,
    plugin_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    short_name TEXT NOT NULL,
    parent_id TEXT REFERENCES entities(id),
    source_file_id TEXT REFERENCES entities(id),
    source_byte_start INTEGER,
    source_byte_end INTEGER,
    source_line_start INTEGER,
    source_line_end INTEGER,
    properties TEXT NOT NULL,
    content_hash TEXT,
    summary TEXT,
    wardline TEXT,
    first_seen_commit TEXT,   -- underlying git SHA (no `-dirty` suffix); set at run boundary
    last_seen_commit TEXT,    -- underlying git SHA; updated every run that observes the entity
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX ix_entities_last_seen_commit ON entities(last_seen_commit);
CREATE INDEX ix_entities_kind ON entities(kind);
CREATE INDEX ix_entities_plugin_kind ON entities(plugin_id, kind);
CREATE INDEX ix_entities_parent ON entities(parent_id);
CREATE INDEX ix_entities_source_file ON entities(source_file_id);
CREATE INDEX ix_entities_content_hash ON entities(content_hash);

-- Tags (denormalised)
CREATE TABLE entity_tags (
    -- `plugin_id` is the namespace that emitted/owns the free-form tag.
    -- Entity IDs already carry an entity owner, but multiple plugins may tag
    -- the same core or sibling entity; tag ownership is therefore explicit.
    entity_id TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    plugin_id TEXT NOT NULL,
    tag TEXT NOT NULL,
    PRIMARY KEY (entity_id, plugin_id, tag)
);
CREATE INDEX ix_entity_tags_tag ON entity_tags(tag);
CREATE INDEX ix_entity_tags_plugin_tag ON entity_tags(plugin_id, tag);

-- Edges. Natural PK (kind, from_id, to_id) per ADR-026: two edges of the
-- same kind between the same entities collapse into one even when their
-- properties differ. The properties bag is evidence/metadata, not identity.
CREATE TABLE edges (
    kind               TEXT NOT NULL,
    from_id            TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    to_id              TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    properties         TEXT,
    source_file_id     TEXT REFERENCES entities(id),
    source_byte_start INTEGER,
    source_byte_end   INTEGER,
    confidence         TEXT NOT NULL DEFAULT 'resolved'
                       CHECK (confidence IN ('resolved', 'ambiguous', 'inferred')),
    PRIMARY KEY (kind, from_id, to_id)
) WITHOUT ROWID;
CREATE INDEX ix_edges_from_kind       ON edges(from_id, kind);
CREATE INDEX ix_edges_to_kind         ON edges(to_id, kind);
CREATE INDEX ix_edges_kind            ON edges(kind);
CREATE INDEX ix_edges_kind_confidence ON edges(kind, confidence);

-- Findings
CREATE TABLE findings (
    id TEXT PRIMARY KEY,
    tool TEXT NOT NULL, tool_version TEXT NOT NULL,
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    rule_id TEXT NOT NULL,
    -- Closed core-owned vocabulary per ADR-031; values per ADR-004.
    kind TEXT NOT NULL
         CHECK (kind IN ('defect', 'fact', 'classification', 'metric', 'suggestion')),
    -- Closed core-owned vocabulary per ADR-031; values per ADR-017.
    severity TEXT NOT NULL
             CHECK (severity IN ('INFO', 'WARN', 'ERROR', 'CRITICAL', 'NONE')),
    confidence REAL,
    confidence_basis TEXT,
    entity_id TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    related_entities TEXT NOT NULL,
    message TEXT NOT NULL,
    evidence TEXT NOT NULL,
    properties TEXT NOT NULL,
    supports TEXT NOT NULL,
    supported_by TEXT NOT NULL,
    -- Closed core-owned vocabulary per ADR-031; finding lifecycle states.
    status TEXT NOT NULL
           CHECK (status IN ('open', 'acknowledged', 'suppressed', 'promoted_to_issue')),
    suppression_reason TEXT,
    filigree_issue_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX ix_findings_entity ON findings(entity_id);
CREATE INDEX ix_findings_rule ON findings(rule_id);
CREATE INDEX ix_findings_tool_rule ON findings(tool, rule_id);
CREATE INDEX ix_findings_run ON findings(run_id);
CREATE INDEX ix_findings_status ON findings(status);

-- Summary cache
CREATE TABLE summary_cache (
    entity_id TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    prompt_template_id TEXT NOT NULL,
    model_tier TEXT NOT NULL,
    guidance_fingerprint TEXT NOT NULL,
    summary_json TEXT NOT NULL,
    cost_usd REAL NOT NULL,
    tokens_input INTEGER NOT NULL,
    tokens_output INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint)
);

-- Runs (provenance)
CREATE TABLE runs (
    id TEXT PRIMARY KEY,
    started_at TEXT NOT NULL, completed_at TEXT,
    config TEXT NOT NULL,
    stats TEXT NOT NULL,
    -- Closed core-owned vocabulary per ADR-031; terminal values from the
    -- writer-actor `RunStatus` enum; 'running' is the BeginRun in-flight literal.
    status TEXT NOT NULL
           CHECK (status IN ('running', 'skipped_no_plugins', 'completed', 'failed'))
);

-- FTS5 for text search
CREATE VIRTUAL TABLE entity_fts USING fts5(
    entity_id UNINDEXED,
    name, short_name, summary_text, content_text,
    tokenize = 'porter unicode61'
);

-- FTS5 triggers keep entity_fts synchronised with entities.
-- summary_text is derived from the briefing's purpose + patterns + risks
-- (short textual projection); content_text is populated on demand by the
-- plugin during Phase 1 via the `file_analyzed` message.
CREATE TRIGGER entities_ai AFTER INSERT ON entities BEGIN
    INSERT INTO entity_fts (entity_id, name, short_name, summary_text, content_text)
    VALUES (
        new.id,
        new.name,
        new.short_name,
        COALESCE(json_extract(new.summary, '$.briefing.purpose'), ''),
        ''
    );
END;
CREATE TRIGGER entities_au AFTER UPDATE ON entities BEGIN
    UPDATE entity_fts
    SET name = new.name,
        short_name = new.short_name,
        summary_text = COALESCE(json_extract(new.summary, '$.briefing.purpose'), '')
    WHERE entity_id = new.id;
END;
CREATE TRIGGER entities_ad AFTER DELETE ON entities BEGIN
    DELETE FROM entity_fts WHERE entity_id = old.id;
END;

-- Generated columns + indices for hot JSON properties. These avoid
-- json_extract() in WHERE clauses for frequent filters.
-- scope_level + scope_rank pair: TEXT for equality filters; INTEGER (CASE-
-- mapped per ADR-024) for ordered queries. The semantic ordering
-- project→subsystem→package→module→class→function is non-lexicographic, so a
-- TEXT-only index cannot serve ORDER BY correctly.
ALTER TABLE entities ADD COLUMN scope_level TEXT
    GENERATED ALWAYS AS (json_extract(properties, '$.scope_level')) VIRTUAL;
ALTER TABLE entities ADD COLUMN scope_rank INTEGER
    GENERATED ALWAYS AS (
        CASE json_extract(properties, '$.scope_level')
            WHEN 'project'   THEN 1
            WHEN 'subsystem' THEN 2
            WHEN 'package'   THEN 3
            WHEN 'module'    THEN 4
            WHEN 'class'     THEN 5
            WHEN 'function'  THEN 6
        END
    ) VIRTUAL;
CREATE INDEX ix_entities_scope_rank ON entities(scope_rank) WHERE scope_rank IS NOT NULL;

ALTER TABLE entities ADD COLUMN git_churn_count INTEGER
    GENERATED ALWAYS AS (json_extract(properties, '$.git_churn_count')) VIRTUAL;
CREATE INDEX ix_entities_churn ON entities(git_churn_count) WHERE git_churn_count IS NOT NULL;

-- View for guidance resolver
CREATE VIEW guidance_sheets AS
SELECT id, name,
       json_extract(properties, '$.scope_level') AS scope_level,
       json_extract(properties, '$.scope.query_types') AS query_types,
       json_extract(properties, '$.scope.token_budget') AS token_budget,
       json_extract(properties, '$.match_rules') AS match_rules,
       json_extract(properties, '$.content') AS content,
       json_extract(properties, '$.expires') AS expires,
       tags
FROM entities WHERE kind = 'guidance';
```

### Concurrency

**Writer-actor model** (single writer task owns the sole write connection; all other tasks submit mutations through a bounded channel):

- WAL mode: `PRAGMA journal_mode = WAL`, `synchronous = NORMAL`, `busy_timeout = 5000ms`, `wal_autocheckpoint = 1000` (default).
- `clarion analyze` and `clarion serve` each instantiate exactly one **writer actor** task (a `tokio::task` owning a dedicated `rusqlite::Connection`). All mutations route through a bounded `mpsc::Sender<WriteOp>` with backpressure. There is no in-process write contention; there are no cross-process writers because `clarion analyze` and `clarion serve` don't run the same DB concurrently in v0.1 (see operational posture below).
- **Transaction scope**: `clarion analyze` commits on a rolling boundary of **N files per transaction** (default `N=50`, configurable via `clarion.yaml:storage.tx_batch_size`). This keeps the WAL bounded, lets checkpointing run between batches, and makes `--resume` checkpoints meaningful. A full-batch single transaction is explicitly not used.
- **Consult-mode writes** (summary cache, session state) during `clarion serve` are dispatched on the same writer actor; they interleave with analyze-time writes if a user starts `clarion analyze` against a running `clarion serve` (not recommended but survivable). Writes are applied in arrival order; no starvation because consult writes are tiny and sparse.
- **Readers** (plugin processes, MCP tool calls, HTTP API handlers, the markdown renderer) open read-only `rusqlite` connections from a `deadpool-sqlite` pool (configurable max: default 16). WAL lets them read against the committed snapshot without blocking writers.
- **Checkpointing**: truncate-mode checkpoint issued after each 10 analyze-transactions or after `clarion analyze` completes, whichever comes first.
- **Operational posture (v0.1)**: running `clarion analyze` and `clarion serve` against the same `.clarion/clarion.db` simultaneously is supported but `clarion serve` will observe stale read-snapshots until the analyze finishes and checkpoint completes. `clarion serve` emits a `CLA-INFRA-STALE-SNAPSHOT` finding when this is detected. Users wanting zero-stale reads during long analyze runs should prefer the "shadow DB + atomic swap" pattern (analyze writes to `.clarion/clarion.db.new`, atomic rename on completion) — available via `clarion analyze --shadow-db` flag.

**Why not a single write transaction for the whole batch**: long transactions pin the WAL and prevent checkpoint; WAL growth is unbounded; readers pinned to the pre-analyse snapshot can't advance; SQLite `database is locked` errors surface to consult-mode writes. Per-batch transactions are the industry-standard posture for this workload.

### Scale estimate for elspeth

- File entities: ~1,100
- Code entities (functions, classes, globals, modules, packages): ~100k–200k
- Subsystems: ~20–50
- Edges: ~500k–1M
- Findings in v0.1: hundreds to low thousands (mostly facts)
- Summary cache: ~200k rows (at one summary per entity per fingerprint)
- Expected DB size: 500MB–2GB
- Read latencies: sub-millisecond for indexed lookups

### File layout

```
<project>/.clarion/
    clarion.db              # main store (WAL files beside it)
    config.json             # internal state: schema version, last run IDs
    clarion.log             # structured log
    runs/
        <run_id>/
            config.yaml     # snapshot of clarion.yaml at run time
            log.jsonl       # per-run log
            stats.json      # run statistics
            partial.json    # present if run ended partial

~/.config/clarion/          # user-level
    providers.toml          # API keys, model tier mappings
    plugins.toml            # plugin registry
    defaults.yaml           # default policy overrides
```

`.clarion/` is checked into git (consistent with Filigree's pattern and with the "shared analysis state" principle). SQLite files can diff poorly, so v0.1 ships **two features** for multi-developer teams to handle the committed DB:

- `clarion db export --textual <out_dir>` — emits a deterministic JSON tree: `entities.jsonl` (one entity per line, sorted by id), `edges.jsonl` (sorted by `(kind, from_id, to_id)`), `guidance.jsonl` (sorted by id), `findings.jsonl` (sorted by id). Summary cache is **excluded** (re-derivable on next run, and JSON-diffing thousands of LLM-generated briefings is not useful). Output is git-friendly: a one-entity change produces a one-line diff.
- `clarion db merge-helper <ours.db> <theirs.db> <base.db?> --output merged.db` — applied as a Git merge driver or manually during conflict resolution. Strategy: textual export of each side, deterministic union of entities/edges (last-writer-wins on conflicts keyed by `updated_at`), guidance-sheet conflict surfaced with a `CONFLICT` marker per affected sheet (human must resolve), summary cache cleared (will rebuild).

Users can opt out entirely: `clarion.yaml:storage.commit_db: false` excludes the DB from the commit and Clarion emits `clarion db sync push/pull` commands (v0.1: scp-based; v0.2: S3/git-lfs/HTTP), but the default is still commit-the-DB because the solo-developer and small-team cases benefit from having briefings versioned alongside the code they describe.

**Git merge-driver registration** (optional, recommended for teams):

```
# .gitattributes
.clarion/clarion.db merge=clarion-db

# .git/config (or per-developer)
[merge "clarion-db"]
    name = Clarion DB merger
    driver = clarion db merge-helper --output %A %A %B %O
```

With the driver registered, conflicting runs from two developers produce a deterministic merged DB at commit time; without it, operators resolve manually via `clarion db export --textual` on both sides plus `clarion db import --textual <merged_dir>`.

**Git-commit caveats for `.clarion/clarion.db`**:

- LLM-derived content (briefings, guidance body text) lives in the DB and is therefore committed. Content derived from source files redacted by the pre-ingest secret scanner never reaches the LLM in the first place, so briefings don't contain secret material. Briefings that *describe* security-sensitive code (e.g., "this module is the JWT verifier") are fine to commit — they're public documentation.
- `runs/<run_id>/log.jsonl` records raw LLM request/response bodies for audit. This log is **excluded** from git by default via `.clarion/.gitignore` (`runs/*/log.jsonl`) because those bodies may contain source excerpts that are fine to ship to Anthropic but not appropriate to commit to a public repo. Users opting in to commit run logs must accept that posture explicitly.
- Operational rollouts where the DB is private-not-shared (single-developer experiments, pre-publication audits) can set `clarion.yaml:storage.commit_db: false` and the DB is `.gitignore`'d instead.

### Migration strategy

- Migrations as numbered files embedded in the binary (`refinery` or similar).
- Applied on every `clarion analyze` / `clarion serve` startup.
- Never drop data without explicit `clarion db migrate --destructive`.
- Schema version recorded in `config.json` and a `_schema_version` table.

### What the store does NOT hold

- Raw source code (stored via reference; plugins read files on demand).
- Compiled ASTs (plugins regenerate per session).
- Raw LLM API responses (logged to `runs/<run_id>/log.jsonl` for audit, not the main store).
- Filigree issue content (Clarion holds only association IDs; Filigree is authoritative for issue data).

---

## 4. Policy Engine — Config & Caching Internals

### Full `clarion.yaml` example

```yaml
version: 1

llm_policy:
  profile: default                   # default | budget | deep | custom

  levels:
    function:
      mode: on_demand                # batch | on_demand | off
      model_tier: haiku
      summary_length: short
    class:
      mode: batch
      model_tier: haiku
      summary_length: medium
    global:
      mode: batch
      model_tier: haiku
      summary_length: short
      filter:
        include_if_any:
          - non_trivial_init
          - is_dependency_hub
          - has_wardline_sensitive_type
          - has_decorator
          - has_explicit_annotation
    module:
      mode: batch
      model_tier: sonnet
      summary_length: medium
    subsystem:
      mode: batch
      model_tier: opus
      summary_length: full
    cross_cutting:
      wardline_delta: { mode: off }   # v0.2
      quality:        { mode: off }
      security:       { mode: off }

  overrides:
    - match: { path: "src/generated/**" }
      levels: { all: { mode: off } }
    - match: { path: "tests/**" }
      levels:
        function: { mode: off }
        class:    { mode: off }
        global:   { mode: off }
    - match: { subsystem: "auth" }
      levels:
        subsystem: { model_tier: opus, summary_length: exhaustive }

  budget:
    max_usd_per_run: 50.0
    max_minutes: 120
    on_exceed: warn                  # warn | stop
    dry_run_first: true
    estimate_precision: high

  caching:
    invalidate_on:
      - content_change
      - prompt_template_change
      - model_upgrade
      - guidance_change
    max_age_days: 180                      # TTL backstop: see "Semantic staleness" below
    neighborhood_drift_threshold: 0.5      # flag entries where fan-in/out changed by >50%

providers:
  anthropic:
    api_key_env: ANTHROPIC_API_KEY
    # Tier mapping pinned against the anthropic Python SDK / Rust SDK that Clarion
    # builds against. These IDs must be verified against the SDK's documented
    # model-ID list on each Clarion release; a mismatch fails every LLM call.
    # The implementation plan records the SDK version and CI guards the match.
    tier_mapping:
      haiku:   claude-haiku-4-5        # latest Haiku 4.x as of 2026-04
      sonnet:  claude-sonnet-4-6       # latest Sonnet 4.x as of 2026-04
      opus:    claude-opus-4-7         # latest Opus 4.x as of 2026-04
    timeout_seconds: 60
    max_retries: 3

analysis:
  include:
    - "src/**/*.py"
  exclude:
    - "**/__pycache__/**"
    - "**/.venv/**"
  plugins: [python]
  clustering:
    algorithm: leiden                # leiden | weighted_components
    edge_types: [imports, calls]
    weight_by: reference_count
    min_cluster_size: 3
    weak_modularity_threshold: 0.3

integrations:
  filigree:
    enabled: true
    server_url: "stdio:filigree-mcp"
    emit_observations: true
    emit_findings: true              # v0.1: posts to POST /api/v1/scan-results (Filigree-native schema; see §7)
  wardline:
    enabled: true
    manifest_path: "wardline.yaml"
    overlay_search: "src/**/wardline.overlay.yaml"
    ingest_only: true                # v0.1; v0.2 consumes wardline's findings
```

### Profile presets

| Profile | Philosophy | Function | Class | Global | Module | Subsystem |
|---|---|---|---|---|---|---|
| `budget` | Minimise cost | off | on_demand, haiku | batch-filtered, haiku | batch, haiku | batch, sonnet |
| `default` | Balanced | on_demand, haiku | batch, haiku | batch-filtered, haiku | batch, sonnet | batch, opus |
| `deep` | Maximum depth | batch, haiku | batch, sonnet | batch-filtered, sonnet | batch, opus | batch, opus (exhaustive) |
| `custom` | Pure overrides | user sets everything | | | | |

### Summary cache key design

Cache key: `(entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint)`. Any change to any component → cache miss → fresh LLM call. All components tracked automatically; **syntactic** staleness is impossible.

**Semantic staleness** — three known paths the key alone does not see:

1. **Graph-neighborhood drift**: if an entity's call-graph neighborhood shifts materially (e.g., it becomes a hot path) without its own text changing, its `risks` and `relationships` fields may be out of date. Invalidation trigger: store `caller_count` and `fan_out` in the summary cache row; flag cache entries whose neighborhood has shifted by more than 50% (configurable via `clarion.yaml:llm_policy.caching.neighborhood_drift_threshold`) as `stale_semantic: true`. Flagged entries are served with a header indicating staleness; the next `clarion analyze` refreshes them.

2. **Model-identity drift**: a tier name (`sonnet`) mapping to a new concrete model version (`claude-sonnet-4-6` → `claude-sonnet-4-7`). The cache row already stores the concrete model; the tier resolver compares on write and treats a mismatch as a cache miss. No special handling needed — this is already correct.

3. **Guidance-worldview drift**: a guidance sheet whose text is unchanged but whose underlying assumptions have gone stale. No automated signal; surfaced via staleness review and git-churn-based findings. `reviewed_at` gives operators a handle.

**TTL backstop**: summary cache rows older than `clarion.yaml:llm_policy.caching.max_age_days` (default: 180 days) are invalidated unconditionally on next query. This bounds the time a silently-stale briefing can influence agents; 180 days is long enough that cache hit rates stay high during active development and short enough that stale models don't persist across a full Anthropic model generation.

**TTL interacts asymmetrically with stale pinned guidance**, which is a sharp edge worth naming: invalidating a briefing at day 180 forces a fresh LLM call — but that fresh call still composes the *same* stale `pinned: true` guidance sheet if the operator hasn't re-reviewed it. The briefing is fresh; its framing is not. v0.1 mitigations:

1. **Churn-triggered cache invalidation for guidance-stale entities**: when `CLA-FACT-GUIDANCE-CHURN-STALE` fires (stale pinned sheet on a high-churn entity), the summary cache rows whose `guidance_fingerprint` includes that sheet are invalidated eagerly, not at TTL. The operator now sees churn-stale findings *and* feels pressure to act because cache misses start accruing cost.
2. **Stale-guidance flag on briefings**: briefings whose composed guidance includes a sheet with a `CLA-FACT-GUIDANCE-CHURN-STALE` or `CLA-FACT-GUIDANCE-EXPIRED` finding against it carry `briefing_guidance_may_be_stale: true` in the response envelope. Agents consuming the briefing can downweight its `risks` / `patterns` claims accordingly.

---

## 5. Pipeline — Rule Catalogue & Example Run

### Phase-7 structural findings (v0.1)

These rules combine signals Clarion uniquely holds — clusters from Phase 3, Wardline tier declarations from Wardline ingest, and the prior-run entity set — into findings that neither Wardline nor Filigree can compute alone:

| Rule | Severity | Confidence basis | Signal |
|---|---|---|---|
| `CLA-FACT-TIER-SUBSYSTEM-MIXING` | WARN | heuristic | A subsystem has members declared across disagreeing tiers (e.g., 11 members `INTEGRAL`, 3 `GUARDED`). Either a misclassification Wardline can't see or a latent tier boundary worth naming. Emitted against the subsystem entity with `related_entities` listing the outliers. |
| `CLA-FACT-ENTITY-DELETED` | INFO | deterministic | Entity present in the previous run's catalog is absent in this run. Compared against prior run's entity set at Phase-7 boundary. Emitted per deleted entity. Surfaces silently orphaned Filigree issues, silently-no-op guidance sheets, and persistent-until-TTL cache rows. |
| `CLA-FACT-SUBSYSTEM-TIER-UNANIMOUS` | INFO (fact) | deterministic | Subsystem members share a uniform declared tier. Useful positive signal for tier-consistency reports; cheap companion to the mixing rule. |

**Why these belong in Phase 7, not the plugin's Phase-1 emission**: the rules depend on clustering output (Phase 3) and prior-run state, which are core-side concerns. Emitting them from the plugin would require the plugin to know about subsystems and prior runs — violation of Principle 3.

**`CLA-FACT-TIER-SUBSYSTEM-MIXING` threshold**: default `min_outlier_count: 2` and `min_outlier_fraction: 0.1` (i.e., at least 2 outliers AND at least 10% of subsystem members). Configurable via `clarion.yaml:analysis.clustering.tier_mixing_thresholds`. Tuned to avoid flagging subsystems where a single outlier is legitimate boundary-infrastructure (e.g., an `EXTERNAL_RAW` parser adjacent to an otherwise-`INTEGRAL` validation subsystem).

### Pre-ingest secret-scanning findings (WP5)

These core-emitted rules are the ADR-013 audit surface. ADR-013 remains canonical for scanner behaviour; this catalogue records the emitted finding IDs.

| Rule | Severity | Category | Description | Remediation | ADR |
|---|---|---|---|---|---|
| `CLA-SEC-SECRET-DETECTED` | ERROR | security | Pre-ingest secret scanner detected a credential pattern in a file slated for LLM dispatch. | Remove the secret, rotate the credential, or whitelist via `.clarion/secrets-baseline.yaml` with a justification. | [ADR-013](../adr/ADR-013-pre-ingest-secret-scanner.md) |
| `CLA-SEC-UNREDACTED-SECRETS-ALLOWED` | ERROR | security | Operator invoked `--allow-unredacted-secrets`; file content reached the LLM provider with secrets intact. | Audit override usage via `filigree list --rule-id=CLA-SEC-UNREDACTED-SECRETS-ALLOWED --since 30d`. | [ADR-013](../adr/ADR-013-pre-ingest-secret-scanner.md) |
| `CLA-INFRA-SECRET-BASELINE-NO-JUSTIFICATION` | ERROR | infra | Baseline entry missing required `justification` field; entry not honoured. | Add a `justification` string explaining why the match is safe. | [ADR-013](../adr/ADR-013-pre-ingest-secret-scanner.md) |
| `CLA-INFRA-SECRET-BASELINE-MATCH` | INFO | infra | Baseline entry suppressed a scanner detection as an audit event. | None; informational and retained for `NFR-SEC-04` audit. | [ADR-013](../adr/ADR-013-pre-ingest-secret-scanner.md) |
| `CLA-INFRA-SECRET-OVERRIDE-UNCONFIRMED` | ERROR | infra | `--allow-unredacted-secrets` supplied without confirmation; run aborted before start. | Supply `--confirm-allow-unredacted-secrets=yes-i-understand` in non-TTY contexts or run interactively. | [ADR-013](../adr/ADR-013-pre-ingest-secret-scanner.md) |

### Entity-set diff (deletion detection)

At Phase 7, Clarion compares the current run's entity IDs against the prior run's set (read from the prior `run_id`'s stats; if no prior run exists, this phase is a no-op). For each entity ID present before and absent now:

- Emit `CLA-FACT-ENTITY-DELETED` against a synthetic deletion marker (`core:deleted:{former_entity_id}`).
- Surface on Filigree the deletion so that Filigree issues carrying the orphan ID can be triaged.
- Guidance sheets with explicit-entity `match_rules` pointing at the deleted ID emit `CLA-FACT-GUIDANCE-ORPHAN` with confidence: deterministic and with a pointer to the deleted-entity finding.
- Summary cache rows for the deleted entity are invalidated (no new briefings can be produced; reads against the old ID return a deletion marker).

Silent-fallback principle compliance: before Rev 4 this path was silent. Now every deletion produces two findings (the deletion itself and an orphan finding per affected guidance sheet) so operators cannot miss the transition.

### Failure & degradation (full table)

| Failure | Recovery |
|---|---|
| Plugin parse error on file | `CLA-PY-PARSE-ERROR`; skip file; continue |
| Plugin timeout (default 30s) | `CLA-PY-TIMEOUT`; skip file; continue |
| Plugin process crash | Core restarts plugin; resumes at next file; `CLA-INFRA-PLUGIN-CRASH` |
| LLM rate limit | Exponential backoff with jitter; retry up to `max_retries` |
| LLM non-transient error | `CLA-INFRA-LLM-ERROR`; skip entity; continue |
| Budget exceeded (`on_exceed: warn`) | Log warning; emit `CLA-INFRA-BUDGET-WARNING`; continue |
| Budget exceeded (`on_exceed: stop`) | Halt dispatch; emit `CLA-INFRA-BUDGET-EXCEEDED`; write partial manifest |
| Plugin crashes >10% of files | Abort; `CLA-INFRA-ANALYSIS-ABORTED`; run marked failed |

No silent fallbacks. Every failure produces a finding.

### Example run

```
$ clarion analyze /home/john/elspeth
  Reading config from /home/john/elspeth/clarion.yaml ✓
  Loading plugin: python (v0.1.0) ✓
  Phase 0: discovery + dry-run
    Files: 1,126 Python files
    Estimated cost: $11.80 ± $2.40
    Proceed? [y/N] y
  Phase 1: structural extraction     [████████████████] 1,126/1,126   0:04:12
  Phase 1.5: enrichment              ✓ (28s)
  Phase 2: graph completion          ✓ (4.3s, 187,420 entities, 623,491 edges)
  Phase 3: clustering                ✓ (0.8s, 43 subsystems proposed)
  Phase 4: leaf (haiku)              [████████████████]  8,421/8,421  0:17:30  $2.14
  Phase 5: module (sonnet)           [████████████████]  1,126/1,126  0:09:45  $4.32
  Phase 6: subsystem (opus)          [████████████████]     43/43     0:06:18  $4.91
  Phase 7: cross-cutting (wardline ingest) ✓ (0.4s)
  Phase 8: emission
    Catalog: .clarion/catalog.json (4.1 MB)
    Markdown: .clarion/catalog/*.md (51 files)
    Findings: 137 (127 facts, 10 defects)
    Filigree observations pushed: 2
  Done in 0:38:12, total cost $11.37
  Run ID: run-2026-04-17-153002
```

---

## 6. MCP Tool Catalogue (Exact Tool Names)

#### Navigation
- `goto(id)`, `goto_path(path, line?)`, `back()`, `zoom_out()`, `zoom_in(child_id)`, `breadcrumbs()`

#### Inspection
- `summary(id?, detail?)` — structured briefing
- `source(id?, range?)`
- `metadata(id?)` — raw properties, tags, etc.
- `guidance_for(id?)` — composed guidance stack
- `findings_for(id?, filter?)`
- `wardline_for(id?)` — declared tier/groups/contracts

#### Neighbours
- `neighbors(id?, edge_kind?, direction?)`
- `callers(id?)`, `callees(id?)`
- `children(id?, kind_filter?)`
- `imports_from(id?)`, `imported_by(id?)`
- `in_subsystem(id?)`, `subsystem_members(id?)`

#### Search
- `search_structural(query)`
- `search_semantic(query, limit?)`
- `find_by_tag(tag, scope?)`
- `find_by_wardline(tier?, group?)`
- `find_by_kind(kind, scope?)`

#### Findings & observability
- `list_findings(filter)`
- `emit_observation(id, text)` — creates filigree observation
- `promote_observation(obs_id, issue_template?)` — promotes to filigree issue
- `cost_report(since?)`

#### Guidance
- `show_guidance(id?)`
- `list_guidance(filter?)`
- `propose_guidance(id, content, rules?)`
- `promote_guidance(obs_id)`

#### Scope / session
- `set_scope_lens(lens)`
- `session_info()`

#### Exploration-elimination shortcuts (Principle 2)

All `scope?` parameters accept either an `EntityId` (confine results to descendants of that entity, typically a subsystem or package) or a path glob (`"src/auth/**"`). Omitted → whole project.

- `find_entry_points(scope?)`
- `find_http_routes(scope?)`
- `find_cli_commands(scope?)`
- `find_data_models(scope?)`
- `find_config_loaders(scope?)`
- `find_tests(scope?)`, `find_fixtures(scope?)`
- `find_deprecations(scope?)`
- `find_todos(scope?)`
- `find_dead_code(scope?)`
- `find_circular_imports(scope?)`
- `find_coupling_hotspots(scope?)`
- `recently_changed(since?, scope?)` — `since?` accepts ISO 8601 timestamp or relative (`"7d"`, `"2w"`)
- `high_churn(limit?, scope?)` — `limit?` defaults to 20
- `what_tests_this(id)` — required `EntityId`; returns test functions whose imports reach this entity's module/symbol

### Session state (Rust shape)

```rust
struct Session {
    id: SessionId,
    client: ClientInfo,
    created_at, last_seen_at: DateTime,

    cursor: Option<EntityId>,          // current "here I am" position
    breadcrumbs: Vec<EntityId>,        // nav history
    scope_lens: ScopeLens,             // filter/orientation for neighbour queries

    session_cost: CostAccumulator,
    proposed_guidance: Vec<ProposalId>,
    emitted_observations: Vec<ObsId>,
}

enum ScopeLens {
    Structural,       // callers, callees, contains, contained_in
    Taint,            // follow tier-flow paths (v0.2 via wardline findings)
    Subsystem,        // stay within the current subsystem
    Wardline,         // follow declared-boundary-contract chains
}
```

### Response envelope

```json
{
    "result": { /* tool-specific data */ },
    "cursor": "<updated cursor EntityId if navigation>",
    "briefing": {
        "purpose": "...",
        "maturity": "stable",
        "risks": [...],
        "patterns": [...],
        "antipatterns": [...],
        "relationships": {...}
    }
}
```

---

## 7. Integrations — Implementation Detail

### Severity mapping (Clarion internal ↔ Filigree wire)

Clarion's finding records use an internal `{INFO, WARN, ERROR, CRITICAL}` vocabulary for defects plus `NONE` for facts; Filigree's intake accepts `{critical, high, medium, low, info}`. Non-matching values are coerced to `"info"` with a warning in the response. The mapping:

| Clarion internal | Filigree wire | Reverse (on read-back) |
|---|---|---|
| `CRITICAL` | `critical` | `CRITICAL` |
| `ERROR` | `high` | `ERROR` |
| `WARN` | `medium` | `WARN` |
| `INFO` | `info` | `INFO` |
| `NONE` (facts) | `info` (with `metadata.clarion.kind = "fact"`) | `NONE` |

The Clarion internal value is preserved in `metadata.clarion.internal_severity` for lossless round-trip. Clarion's read-back path consults `metadata.clarion.internal_severity` first and falls back to `severity` only if the metadata is absent (e.g., for findings ingested from Wardline or other sources that don't set Clarion-specific metadata).

### Rule-ID round-trip policy

Rule IDs are namespaced per-tool and free-form within a namespace:

- `CLA-PY-STRUCTURE-001`, `CLA-FACT-ENTRYPOINT`, `CLA-INFRA-*`, `CLA-SEC-*` (Clarion)
- `PY-WL-001-GOVERNED-DEFAULT`, `SUP-001-*`, `SCN-*`, `TOOL-ERROR` (Wardline — verified against `scanner/rules/` and `wardline-sarif-extensions.schema.json`)
- `COV-*` (future coverage scanner)
- `SEC-*` (future security scanner)

Filigree's `rule_id` column is a free-text string — no enum enforcement. Clarion's and Wardline's rule IDs round-trip byte-for-byte. Clarion's consult tools re-namespace on display: findings with `scan_source="wardline"` surface as "Wardline/PY-WL-001-GOVERNED-DEFAULT"; findings with `scan_source="clarion"` surface unprefixed. The rule-ID prefix convention is a suite-wide social convention; there is no code enforcement.

### `scan_run_id` lifecycle

Filigree's `scan_run_id` is optional on ingest. If absent, findings are inserted without run-lifecycle tracking. If present and `complete_scan_run=true`, Filigree closes the run on receipt; unknown IDs log a warning and proceed.

**Clarion v0.1 policy**: Clarion's `run_id` (§2, `Source.run_id`) maps 1:1 onto Filigree's `scan_run_id`. Clarion calls Filigree's `create_scan_run` MCP tool at Phase 0 of `clarion analyze` before posting the first finding batch, and posts with `complete_scan_run=false` on intermediate batches, `complete_scan_run=true` on the final batch. On resume (`clarion analyze --resume`), Clarion re-uses the same `run_id` and posts batches with `mark_unseen=false` to avoid triggering Filigree's "no longer seen" transitions. Failure to create the scan run (e.g., Filigree unreachable) emits `CLA-INFRA-FILIGREE-UNREACHABLE` and Clarion continues posting without a `scan_run_id`, losing run-lifecycle coordination for that `clarion analyze` invocation.

### Dedup collision when entities move within a file

Filigree's dedup key is `(file_id, scan_source, rule_id, coalesce(line_start, -1))`. Two consecutive `clarion analyze` runs emitting the same rule for the same entity at two different line numbers (because the entity moved in the file) produce **two findings**, not one. The older one eventually transitions to `unseen_in_latest` if `mark_unseen=true`; otherwise it persists.

**Clarion v0.1 policy**: rule IDs are *not* synthesised with entity IDs (that would defeat Filigree's cross-run triage). Instead, Clarion posts with `mark_unseen=true` so that old-position findings for the same rule on the same file transition to `unseen_in_latest` when the new run reports them at a different line. Operators reading findings should expect the `unseen_in_latest` status to accumulate for entities that frequently move; `clarion analyze --prune-unseen` removes stale `unseen_in_latest` findings older than 30 days (configurable).

This is an acknowledged coarseness. A v0.2 improvement would push per-entity dedup into Filigree as a server-side option — see §9 Filigree prerequisites (future-state).

### Commit-ref and dirty-tree handling

Wardline SARIF includes `wardline.commitRef` with a `-dirty` suffix when the tree is modified. Clarion adopts the same convention:

- `clarion analyze` records the current HEAD SHA plus a `-dirty` suffix if `git status --porcelain` returns non-empty. Recorded in `runs/<run_id>/stats.json` as `commit_ref`, propagated onto every emitted finding's `metadata.clarion.commit_ref`.
- Dirty-tree runs are permitted (operators frequently run against in-progress code) but emit `CLA-INFRA-DIRTY-TREE-RUN` at INFO severity for provenance.
- `clarion analyze --require-clean` refuses dirty runs; used in CI contexts where provenance must be strict.

### SARIF → Filigree translator

External SARIF is a standing need for the suite regardless of Wardline's emission posture: Semgrep, CodeQL, Trivy, and other third-party scanners all emit SARIF; Filigree's native intake is not SARIF. A **SARIF → Filigree translator is a permanent part of Clarion's feature set**, not a Wardline-bridge workaround.

v0.1 ships `clarion sarif import <sarif_file> [--scan-source <name>]` as a CLI:

- Maps SARIF `result.locations[].physicalLocation` to Filigree `path` + `line_start` + `line_end`.
- Maps SARIF `result.level` (`error` / `warning` / `note`) to Filigree severity via `{error→high, warning→medium, note→info}`.
- Preserves each result's `properties` bag into `metadata.sarif_properties.*`. For files emitted by Wardline, the preservation lands under `metadata.wardline_properties.*` (44 extension keys; literal passthrough) because Wardline's keys use the `wardline.*` namespace convention.
- Uses `--scan-source` to tag the emission (defaults to `wardline` if the SARIF driver name is `wardline`; otherwise the driver name is lowercased).

**Wardline-specific ownership evolves; the translator remains**:

- **v0.1**: Wardline emits SARIF to disk as it does today; operators run `clarion sarif import wardline.sarif --scan-source wardline` (manually or from Clarion's `clarion analyze` post-hook). This is "ownership of the Wardline adapter lives Clarion-side."
- **v0.2+**: Wardline gains a native `POST /api/v1/scan-results` emitter, removing the need for Clarion-side Wardline translation (see §10 ADR-015). **The translator itself stays** — it handles Semgrep, CodeQL, and every other SARIF-emitting tool the project might adopt.

What moves in v0.2 is *who owns the Wardline-specific mapping*, not whether SARIF translation exists. Framing this correctly matters: operators expect a SARIF ingest path indefinitely, and removing `clarion sarif import` when Wardline gains a native emitter would break that expectation for every other SARIF source.

### Wardline state files — in/out decision

Wardline persists ten state files beyond `wardline.yaml`. Recon identified each; v0.1 decision per file:

| File | v0.1 ingest | Rationale |
|---|---|---|
| `wardline.yaml` | YES | Already in scope (manifest: tiers, boundaries, groups). |
| Overlays matching `src/**/wardline.overlay.yaml` | YES | Already in scope; supplementary group-1 and group-17 boundary declarations. |
| `wardline.fingerprint.json` | YES | Authoritative per-function state — aligns directly with Clarion's entity model. |
| `wardline.exceptions.json` | YES | Excepted-finding register with expiry. Clarion tags affected entities with `wardline.excepted` so agents see "this has an active exception, don't flag further" as part of the briefing. |
| `wardline.compliance.json` | NO (v0.2) | Compliance ledger — relevant to governance reports, not yet to Clarion's catalog shape. |
| `wardline.conformance.json` | NO (v0.2) | Conformance gates — similar reasoning. |
| `wardline.perimeter.baseline.json` | NO (v0.2) | Perimeter fingerprint for Wardline's own scans. |
| `wardline.manifest.baseline.json` | NO (v0.2) | Manifest fingerprint for Wardline's own scans. |
| `wardline.retrospective-required.json` | NO (v0.2) | Retrospective state marker — relevant to Wardline reporting, orthogonal to Clarion's briefings. |
| `wardline.sarif.baseline.json` | YES (read-only, for SARIF→Filigree translator) | 663-result baseline; the translator reads this to emit findings to Filigree, not to alter Clarion's catalog directly. |

### HTTP Read API — full endpoint list

```
GET  /api/v1/entities?file=<path>&kind=<kind>&tag=<tag>
GET  /api/v1/entities/<id>
GET  /api/v1/entities/<id>/neighbors?edge=<kind>&direction=<dir>
GET  /api/v1/entities/<id>/summary?detail=<level>
GET  /api/v1/entities/<id>/guidance
GET  /api/v1/entities/<id>/findings
GET  /api/v1/entities/resolve?scheme=<scheme>&value=<value>[&file=<path>]
GET  /api/v1/findings?tool=<tool>&rule=<rule>&kind=<kind>&status=<status>
GET  /api/v1/wardline/declared?scope=<entity_id|path>
GET  /api/v1/state        # { last_analysed_at, commit_sha, is_stale, stats }
GET  /api/v1/health
GET  /api/v1/metrics      # Prometheus-compatible
```

### Entity resolution — schemes accepted

| `scheme` | `value` form | Notes |
|---|---|---|
| `wardline_qualname` | `ScanEngine._scan_file` | Requires `&file=<path>` disambiguator (Wardline qualnames are non-unique across files). |
| `wardline_exception_location` | `src/wardline/scanner/engine.py::ScanEngine._scan_file` | Double-colon separator per Wardline's exception-register convention. |
| `file_path` | `src/auth/tokens.py` | Returns the file entity. |
| `sarif_logical_location` | `auth.tokens.TokenManager.verify` | Matches SARIF `logicalLocations[].fullyQualifiedName`. |

Response:

```json
{
  "entity_id": "python:class:auth.tokens::TokenManager",
  "kind": "class",
  "resolution_confidence": "exact|heuristic|none",
  "alternatives": [/* other entity IDs that matched; empty for exact */]
}
```

**404 behaviour**: returns `resolution_confidence: "none"` with an empty `entity_id` rather than HTTP 404. Lets callers distinguish "Clarion doesn't know this" from "Clarion is down."

### Authentication — full spec (ADR-012)

**Default on Linux / macOS — Unix domain socket** (`serve.auth: uds`):

- `clarion serve` binds `<project_root>/.clarion/socket` with mode `0600`, owner = the UID running `clarion serve`.
- Transport: HTTP/1.1 over UDS. Server: `axum` + `tokio::net::UnixListener`. Clients: `hyper-unix-connector` or equivalent.
- Auth is filesystem-permissions based; HTTP layer does not require `Authorization` header under UDS mode.
- Stale-socket cleanup: on startup, Clarion stat-checks `.clarion/socket` — if present and no process is listening, it unlinks before binding. Double-start (`clarion serve` twice) fails loudly with `CLA-INFRA-SOCKET-IN-USE`.
- Sibling discovery: `.clarion/config.json` records `serve.socket_path`; sibling tools read the path and connect via `unix://<absolute path>`.

**Fallback (auto on Windows, opt-in elsewhere) — TCP + Bearer token** (`serve.auth: token`):

- Windows default: `token` mode on `127.0.0.1:8765` (configurable via `serve.bind_port`).
- Auto-mint: first `clarion serve` writes `.clarion/auth.token` (mode `0600`). Token format: 32 URL-safe base64 bytes (43 chars) prefixed `clrn_` for grep-ability in logs.
- OS-keychain promotion: `clarion serve auth promote-to-keychain` migrates the token from file to macOS Keychain / Linux libsecret / Windows Credential Manager via the `keyring` crate. Emits `CLA-INFRA-TOKEN-STORAGE-DEGRADED` if the keychain is unavailable (falls back to file).
- Wire format: HTTP header `Authorization: Bearer clrn_<43chars>`. Constant-time comparison.
- Rotation: `clarion serve auth rotate` generates a new token, accepts both old and new for 24 hours, drops the old. Rotation is idempotent.
- Scoping: v0.1 has one token per install with full read access. Per-endpoint scoping and revocation lists are v0.2+.

**Explicit-none** (`serve.auth: none` or `--i-accept-no-auth`):

- Allowed but loud: emits `CLA-INFRA-HTTP-AUTH-DISABLED` (severity ERROR) on every `clarion serve` startup and reaches Filigree through the normal finding pipeline. Persistent banner in logs.
- Use cases: air-gapped CI where external ingress is controlled separately; local debugging with explicit operator decision. CLI flag is deliberately verbose (`--i-accept-no-auth`) to prevent accidental muscle-memory enabling.

**How sibling tools pick up auth in CI**:

- **UDS mode**: sibling tools mount the project's `.clarion/socket` into their process (bind-mount for containers, bare path for same-host processes). No token plumbing.
- **Token mode**: `CLARION_TOKEN` env var (preferred for CI; injected via the CI provider's secret store) or `.clarion/auth.token` bind-mount. Wardline's `wardline.yaml` records the Clarion endpoint URL (`unix://…` or `http://127.0.0.1:8765`) but not the token — token comes from env.
- **Pre-flight check**: `clarion check-auth --from wardline` returns exit 0 if the endpoint is reachable and authenticated under the active mode. Returns 0 with warning under `none` mode.

---

## 8. Security — Operator Guidance

Some risks sit outside Clarion's code but inside the operator's responsibility. These belong in the team's onboarding doc, not in the tool's runtime defences:

- **Use project-scoped API keys, not personal ones, when `storage.commit_db: true`.** Briefings in `.clarion/clarion.db` were paid for by whoever ran `clarion analyze`. A teammate pulling your committed DB benefits from LLM calls your personal key paid for. Use an Anthropic project / org key, not your personal key, when committing the DB.
- **Rotate tokens when a committed DB exposes a stale model's output.** If `.clarion/clarion.db` was generated with a leaked or exposed API key, the token is already used; the briefings in the DB are not themselves secret but the key's usage fingerprint is. Rotate, then re-run `clarion analyze` to overwrite the briefing provenance.
- **Review `.clarion/.gitignore` before first commit.** The default excludes `runs/*/log.jsonl` (raw LLM request/response bodies); if operators opt into committing run logs for audit, they accept that source excerpts sent to Anthropic ship to the repo. That's a choice, not an oversight — but it must be a deliberate one.

### Audit-surface finding IDs

Every security-relevant event emits a finding:

- `CLA-SEC-SECRET-DETECTED` — unredacted secret blocked LLM dispatch
- `CLA-SEC-UNREDACTED-SECRETS-ALLOWED` — operator overrode block with `--allow-unredacted-secrets`
- `CLA-INFRA-SECRET-BASELINE-NO-JUSTIFICATION` — baseline entry lacks the required review rationale
- `CLA-INFRA-SECRET-BASELINE-MATCH` — baseline entry suppressed a scanner hit
- `CLA-INFRA-SECRET-OVERRIDE-UNCONFIRMED` — override flag was not confirmed, so the run aborted before start
- `CLA-INFRA-TOKEN-STORAGE-DEGRADED` — OS keychain unavailable, fell back to file-mode `0600`
- `CLA-INFRA-BRIEFING-INVALID` — LLM returned schema-invalid content twice, possible injection
- `CLA-SEC-VOCABULARY-CANDIDATE-NOVEL` — novel `patterns`/`antipatterns` tag proposed by LLM (light signal; mostly harmless)

Findings feed Filigree via the normal exchange. A security-focused operator can `filigree list --label=security --since 7d` across all three tools.

---

## 9. Suite Bootstrap — Prerequisites Detail

Clarion v0.1 is not joining an existing Loom fabric — it is the work that weaves the fabric for the v0.1 suite of Clarion + Filigree + Wardline. The system-design §11 describes Clarion's capability-probe and degraded-mode posture; this section names the specific changes Clarion asks of sibling products as prerequisites. Where Clarion can ship a workaround while the sibling tool catches up, the workaround is noted.

### 9.1 Filigree prerequisites

**Required for Clarion v0.1 ship**:

1. **`registry_backend` config flag + pluggable `RegistryProtocol`** (ADR-014). Default: `filigree` (current behaviour). Alternative: `clarion`. When set to `clarion`, all four `file_records(id)` foreign-key writes route through a `RegistryProtocol` that consults Clarion's HTTP read API for `file_id` resolution before falling back to local auto-create. Three auto-create paths (`POST /api/v1/scan-results`, `create_observation(file_path=…)`, `trigger_scan`) are the primary refactor surface. Surgery estimate from recon: ~5–8 files in Filigree + SQLite FK rework + test updates. **Clarion workaround if absent**: Clarion still emits findings via scan-results; Filigree auto-creates shadow `file_records` under Filigree-native rules; Clarion's "owns the file registry" claim is downgraded to "owns the entity catalog; Filigree shadows the file mapping."

2. **[Deferred to v0.2 per ADR-016]** Observation-creation HTTP endpoint. v0.1 transport is Clarion spawning `filigree mcp` as a subprocess and using the existing `create_observation` MCP tool over stdio. v0.2 adds `POST /api/v1/observations` with a schema parallel to the MCP tool; Clarion's emit path switches to HTTP and the subprocess path retires.

3. **`scan_source` coordination** (ADR-017 supplementary). Filigree's `scan_source` is free-form today; no registry. Adding a `valid_scan_sources` section to `GET /api/files/_schema` (or a `GET /api/v1/scan-sources` endpoint) lets Clarion detect name collisions and Wardline register itself. **Clarion workaround if absent**: hardcoded reserved names (`clarion`, `wardline`, `cov`, `sec`) documented in §7; collisions surface as duplicate finding IDs post-ingest.

4. **Schema-compatibility contract test** (ADR-017 supplementary). Filigree publishes a `tests/fixtures/scan_results_contract.json` representative payload alongside each release; Clarion's CI pins against the contract. Filigree's CHANGELOG (`CHANGELOG.md:91`) already flags `Breaking (API)` changes — a published contract turns social discipline into code enforcement. **Clarion workaround if absent**: Clarion's integration tests run against the current Filigree `main`, treating the `GET /api/files/_schema` output as the contract.

5. **`metadata` nesting convention published** (ADR-017 supplementary). Filigree preserves `metadata` dict contents verbatim but does not index them. Clarion's nesting (`metadata.clarion.*`, `metadata.wardline_properties.*`) is a social convention; publishing it in Filigree docs prevents future scanners from colliding. **Clarion workaround if absent**: convention lives only in Clarion's docs; enforcement is operator vigilance.

**Nice-to-have (v0.2+)**:

- **`POST /api/v1/observations` HTTP endpoint** (ADR-016 retirement trigger) — when Filigree ships this, Clarion's `filigree mcp` subprocess-spawn path retires; Clarion emit switches to HTTP.
- **Server-side per-entity dedup** for scan-results (supersedes Clarion's `mark_unseen` workaround in §7). Adds an optional `entity_id` extension field to Filigree's dedup key.
- **Native SARIF ingest endpoint** (`POST /api/v1/sarif-results`) — makes Clarion's SARIF→Filigree translator unnecessary. Non-trivial work in Filigree (SARIF is large).
- **`filigree --clarion-compat` self-check** that verifies a running Filigree deployment satisfies the `registry_backend: clarion` preconditions.

### 9.2 Wardline prerequisites

**Required for Clarion v0.1 ship**:

1. **Stable `REGISTRY_VERSION` export and direct-import contract** (ADR-018). Wardline's `wardline.core.registry.REGISTRY` is a `MappingProxyType` over 42 canonical decorator names; `REGISTRY_VERSION` is a string. Clarion's Python plugin imports `REGISTRY` directly at startup (no network call; lightweight dependency — `wardline` package in plugin's pipx venv). Wardline commits to: (a) `REGISTRY_VERSION` semver, (b) backward-compatible additions within a major version, (c) `LEGACY_DECORATOR_ALIASES` table maintained for renames. **Clarion workaround if absent**: hardcoded registry mirror in Clarion's plugin, maintained manually — guaranteed to drift.

2. **YAML/JSON descriptor export of `REGISTRY`** — promoted from the earlier v0.2 deferral. Trivial implementation (dump `REGISTRY` to YAML in `wardline annotations descriptor --format yaml`). Inverts the vocabulary coupling: new Wardline annotations land without requiring Clarion plugin release. **Clarion workaround if absent**: direct Python import works for v0.1 because Clarion's Python plugin can require the `wardline` package. Future non-Python plugins (Java, Rust) cannot directly import Python objects; descriptor file is essential for them. Since v0.1 ships only a Python plugin, the workaround is acceptable but the descriptor export is requested for v0.2.

3. **Wardline `scan_source="wardline"` POST to Filigree** (ADR-015 — decision). Two options:

   - **Option A** (recommended for v0.1): Clarion ships a `clarion sarif import <sarif_file>` translator that reads Wardline's SARIF output on disk and POSTs to Filigree with `scan_source="wardline"` and `metadata.wardline_properties.*` preserved. Advantages: no Wardline changes needed; Clarion ships independently. Disadvantages: translator lives in the wrong place (Wardline's domain knowledge in Clarion), Wardline findings appear in Filigree only if someone runs `clarion sarif import`.
   - **Option B**: Wardline gains an HTTP client and native POST. Advantages: clean responsibility boundary; Wardline emits its own findings. Disadvantages: Wardline's `pyproject.toml:22` declares `dependencies = []` — adding `httpx` or `requests` changes the project's dependency posture; not a v0.1 timeline.
   - **v0.1 decision**: Option A. Option B is deferred to v0.2 where Wardline gains an HTTP client as part of the broader "Wardline consumes Clarion" refactor.

4. **`LEGACY_DECORATOR_ALIASES` promise** (ADR-018 supplementary). Clarion's plugin relies on Wardline's alias resolution to avoid double-counting (e.g., `tier_transition` → `trust_boundary`). Wardline commits to adding new renames to this table rather than deleting decorator names outright.

**Nice-to-have (v0.2+)**:

- **Wardline `ProjectIndex` abstraction + HTTP client** to consume Clarion's read API. Large refactor (`ScanEngine`, `ScanContext`, every rule in `scanner/rules/` binds to in-memory AST today). Enables AST-dedup and tier-consistency across the suite. Not a v0.1 timeline; named so the direction is recorded.
- **Qualname in annotation descriptor**. Wardline's descriptor export includes the qualname convention explicitly, simplifying identity reconciliation (§2) from a heuristic map to a direct lookup.
- **Registered Filigree scanner** — `wardline.toml` in `.filigree/scanners/` so operators can `trigger_scan wardline` from Filigree's dashboard. Inversion of the "Wardline is autonomous" model; may or may not align with Wardline's operational posture.

### 9.3 Joint prerequisites (shared ownership)

1. **Cross-tool test fixtures** (§10 Acceptance). Mock Filigree HTTP server (Rust, `wiremock`), Wardline SARIF corpus (committed slice of real output), schema-pin tests. All three tools benefit; naming authority probably sits in Clarion's repo in v0.1, graduating to its own fixture repo if the suite adds a fourth tool.

2. **ADR cross-tool coordination**. Wardline ADRs already reference Filigree issue IDs by string (recon found `wardline-63255c8d5a` cited in `ADR-006-sarif-suppress-as-native-suppression.md:280-281`). Clarion ADRs will similarly reference Wardline and Filigree issues. There's no programmatic cross-repo link; a lightweight cross-tool index in each repo's ADR folder would be a cheap way to keep those references discoverable.

3. **Suite release choreography**. The `registry_backend` flag must land in Filigree before Clarion v0.1 ships; the `REGISTRY_VERSION` export must be stable in Wardline before Clarion v0.1 ships; Clarion's SARIF translator must ship alongside v0.1 rather than later. A three-tool coordinated release plan belongs in the implementation plan, not this design document, but is named here so it doesn't become emergent.

---

## 10. Testing, Observability, Acceptance

### Testing

**Rust core** test pyramid:

| Level | Scope | Target count |
|---|---|---|
| Unit | Pure functions, parsers, policy-engine | 200+ |
| Integration | Store layer, LLM orchestrator (recorded), plugin protocol (fixture plugin) | 60–80 |
| Component | Per-phase pipeline tests | 25–30 |
| End-to-end | `clarion analyze` + `clarion serve` + HTTP API contract | 15–20 |
| Property-based | Graph invariants, ID stability | 10–15 |

**Python plugin** (pytest):

- Unit: AST walkers, filters, emitters, template renderers
- Integration: end-to-end file parsing against small fixtures
- Contract: protocol conformance (plugin responds correctly to every core message)

**Fixtures**:

- `tests/fixtures/tiny/` — 3 modules, ~20 entities
- `tests/fixtures/moderate/` — ~30 modules, realistic shape
- `tests/fixtures/elspeth-slice/` — committed ~50-file elspeth subset (redacted)
- Full elspeth — manual validation + cost benchmarking, not CI

**LLM testing**: `LlmProvider` wraps into a `RecordingProvider` for test mode; records API calls on first run, replays on subsequent runs. Live-LLM integration tests run nightly (not PR CI).

**Determinism**: back-to-back `clarion analyze` on identical fixture → byte-identical entity/edge state. Summaries use recorded responses for reproducibility.

### Observability

- Structured JSON-line logs via `tracing` crate; log rotation at 100MB, 5 files kept.
- `/api/v1/metrics` Prometheus-compatible endpoint.
- Per-run `stats.json` — phases, cost, cache hit ratio, failures.
- `clarion cost --since <date>` CLI report.
- `session_info()` MCP tool for live session state.
- `clarion sessions log <id>` for session-scoped tool-call replay.

### Error surfaces

Every failure produces either a finding or a run-stats entry. Rule-ID namespacing per ADR-017 — `CLA-INFRA-*` for core-emitted pipeline/infrastructure failures; `CLA-PY-*` for Python-plugin-emitted rule findings (including parse errors, which the plugin emits when it fails to parse a file):

- `CLA-PY-PARSE-ERROR` (Python-plugin emitted when a source file fails to parse)
- `CLA-INFRA-PLUGIN-TIMEOUT`, `CLA-INFRA-PLUGIN-CRASH`
- `CLA-INFRA-LLM-ERROR`, `CLA-INFRA-LLM-RATE-LIMIT-EXCEEDED`
- `CLA-INFRA-BUDGET-WARNING`, `CLA-INFRA-BUDGET-EXCEEDED`
- `CLA-INFRA-ANALYSIS-ABORTED`, `CLA-INFRA-BRIEFING-INVALID`
- `CLA-INFRA-MANIFEST-STALE`, `CLA-INFRA-GUIDANCE-EXPIRED`

### Acceptance criteria

**Functional**:

1. `clarion analyze /home/john/elspeth` runs to completion in under 1 hour at ~$15 ± 50%.
2. Every subsystem, module, and per-policy in-scope entity has a briefing.
3. `clarion serve` starts within 2 seconds; MCP `initialize` completes in ≤100ms.
4. A Claude Code consult session navigates 20+ turns without exhausting parent context (parent context growth ≤500 tokens per Clarion tool call average).
5. Guidance sheet edit → dependent summary invalidation → re-query returns fresh content in ≤5 seconds for a hot entity.
6. Wardline-derived guidance regenerates on every analyse; drift surfaced as findings.
7. Filigree observation round-trips: emit → appears in filigree → promotion works.
8. HTTP read API returns correct entity metadata, declared topology, findings.

**Quality**:

1. Core test coverage ≥80% on non-trivial paths; critical paths (protocol, policy engine, migrations) ≥95%.
2. No unexpected `CLA-INFRA-*` findings on clean elspeth run.
3. Determinism test passes.
4. All integration tests pass under `RecordingProvider`.
5. Documentation: installation, `clarion.yaml` schema, plugin-authoring protocol, MCP tool reference, HTTP API reference.

**Operational**:

1. Single-binary: Linux (x86_64, ARM64), macOS (x86_64, ARM64), Windows (x86_64); no dynamic linking beyond libc.
2. Python plugin: `pip install clarion-plugin-python`; Python 3.11+.
3. Store survives unclean shutdown (SIGKILL during analysis).
4. `.clarion/clarion.db` is git-committable and round-trips across machines.

**Ecosystem**:

1. **Mock Wardline client** successfully consumes Clarion's HTTP read API (scope: `entities_for_file`, `declared_topology`, `findings`, `analysis_state`, `entity_summary`). Exists as a test harness in Clarion's repo in v0.1; Wardline's real client is v0.2+.
2. **Mock Filigree HTTP server** (using `wiremock` on Rust) successfully accepts Clarion POSTs with:
   - `metadata.clarion.*` extension fields round-trip verbatim.
   - Severity mapping table applied correctly (INTERNAL→wire).
   - `scan_run_id` lifecycle: create → intermediate posts → complete_scan_run=true.
   - Dedup behaviour under `mark_unseen=true` matches real Filigree.
3. **Wardline SARIF corpus** committed at `tests/fixtures/wardline-sarif/`: at minimum one SARIF file per annotation group + one per Wardline rule. Used for `clarion sarif import` translator tests.
4. **Wardline REGISTRY pin test**: on plugin startup, Clarion verifies `wardline.core.registry.REGISTRY_VERSION` against a pinned expected version; mismatch emits `CLA-INFRA-WARDLINE-REGISTRY-STALE` at severity WARN (not an abort). A test walks through `REGISTRY` and asserts that every canonical decorator name Clarion's plugin expects is present.
5. **Filigree `_schema` pin test**: Clarion's CI fetches `GET /api/files/_schema` from a running Filigree and pins `valid_severities`, `valid_finding_statuses`, `valid_association_types`. Filigree's breaking-change posture (per its `CHANGELOG.md`) makes this the minimum-viable protocol-drift detector.
6. **End-to-end "suite smoke"**: starts Clarion's `clarion serve`, runs Wardline's scanner against a fixture repo, runs `clarion sarif import` on the resulting SARIF, POSTs to a real Filigree instance, verifies that Filigree returns both `scan_source="clarion"` and `scan_source="wardline"` findings with preserved `metadata.*` fields. Requires all three tools co-present; maintained in Clarion's repo as the canonical integration test.
7. **Filigree's `add_file_association`** round-trips Clarion entity IDs (original v0.1 criterion — retained).
8. **Degraded-mode tests**:
   - `clarion analyze --no-filigree` completes, writes `runs/<run_id>/findings.jsonl` locally, emits `CLA-INFRA-FILIGREE-UNAVAILABLE` per finding batch.
   - `clarion analyze --no-wardline` completes, skips Wardline state ingest, annotations emitted with `confidence_basis: "clarion_augmentation"` only.
   - Missing `clarion sarif import` at the system level does not block `clarion analyze`; it only affects Wardline-findings-to-Filigree flow.

---

## 11. Architecture Decisions

Decisions in this design are load-bearing enough that they deserve explicit Architecture Decision Records (ADRs) rather than prose burial. The ADR format is short: **context, decision, alternatives considered, consequences, status**.

**Canonical source**: [system-design.md §12](./system-design.md#12-architecture-decisions) carries the authoritative ADR list with status, priority, and rationale summaries. Authored ADR files live in [../adr/README.md](../adr/README.md).

The table below is a navigation aid for implementers: it maps each ADR to the section(s) of *this detailed design* where the decision shows up concretely. It deliberately does not duplicate the rationale or status columns from system-design.md — consult the canonical table for those.

### Where each ADR is captured in this detailed design

| # | Decision | Where captured |
|---|----------|----------------|
| ADR-001 | Rust for the core | §1, Appendix C |
| ADR-002 | Plugin transport: Content-Length framed JSON-RPC 2.0 subprocess | §1 |
| ADR-003 | Entity ID scheme: symbolic canonical-name; file path as property; EntityAlias v0.2 | §2 |
| ADR-004 | Finding-exchange format: Filigree-native intake; `metadata.clarion.*` nesting | §2, §7 |
| ADR-005 | `.clarion/` git-committable by default | §3 |
| [ADR-006](../adr/ADR-006-clustering-algorithm.md), [ADR-032](../adr/ADR-032-weighted-components-clustering-fallback.md) | Clustering algorithm: Leiden with weighted-components fallback | §4, §5 |
| [ADR-007](../adr/ADR-007-summary-cache-key.md) | Summary cache key design and invalidation | §4 |
| ADR-008 | Superseded by ADR-014 | §7, §9 |
| ADR-009 | Structured briefings vs free-form prose | §2 |
| ADR-010 | MCP as first-class surface | — (system-design §8) |
| [ADR-011](../adr/ADR-011-writer-actor-concurrency.md) | Writer-actor concurrency model | §3 |
| [ADR-012](../adr/ADR-012-http-auth-default.md) | HTTP auth — UDS default with TCP+token fallback | §7 |
| [ADR-013](../adr/ADR-013-pre-ingest-secret-scanner.md) | Pre-ingest secret scanner | §8 |
| ADR-014 | Filigree `registry_backend` flag + pluggable `RegistryProtocol` | §7, §9 |
| [ADR-015](../adr/ADR-015-wardline-filigree-emission.md) | Wardline→Filigree emission: Clarion SARIF translator (v0.1), native Wardline POST (v0.2) | §7, §9 |
| [ADR-016](../adr/ADR-016-observation-transport.md) | Observation transport: `filigree mcp` subprocess (v0.1); `POST /api/v1/observations` HTTP (v0.2 retirement) | §9 |
| [ADR-017](../adr/ADR-017-severity-and-dedup.md) | Severity mapping + rule-ID round-trip + dedup via `mark_unseen=true` | §7 |
| [ADR-018](../adr/ADR-018-identity-reconciliation.md) | Identity reconciliation: Clarion translates; direct `REGISTRY` import with version pinning | §2, §9 |
| ADR-019 | SARIF property-bag preservation: `metadata.wardline_properties.*` pass-through | §7 |
| ADR-020 | Degraded-mode policy: `--no-filigree` / `--no-wardline` flags | — (system-design §11) |
| [ADR-021](../adr/ADR-021-plugin-authority-hybrid.md) | Plugin authority model: hybrid — declared capabilities + core-enforced minimums | §1 (hybrid controls to be added) |
| [ADR-022](../adr/ADR-022-core-plugin-ontology.md) | Core/plugin ontology ownership boundary | §1 (core/plugin split) |

### ADR-001 note (Rust core)

Recorded here for implementer traceability. The amended ADR-001 text lives at [../adr/ADR-001-rust-for-core.md](../adr/ADR-001-rust-for-core.md).

- **Context**: single-binary distribution, LLM orchestration, SQLite + HTTP API workload, multi-plugin subprocess supervision.
- **Decision**: Rust.
- **Alternatives considered**: Go (mature ecosystem, simpler concurrency model, single-binary story, but SQLite ergonomics weaker and subprocess supervision more hand-rolled); TypeScript / Node (fast iteration but single-binary distribution and SQLite concurrency are poor matches).
- **Consequences**: single-binary ship is straightforward; ecosystem (axum, rusqlite, tokio) is mature; Python-plugin interop is out-of-process subprocess and therefore unaffected by language choice; contributor pool narrower than Go or TypeScript.
- **Status**: Accepted.

---

## Appendix A — Future direction: unified storage layer

Once all three systems (Clarion, Wardline, Filigree) are proven and their integration patterns are battle-tested, a unified storage layer across the suite may be worth considering. v0.1 explicitly keeps the stores separate — each tool owns its data, and integration is via protocols (Filigree-native scan-result findings, HTTP read APIs, MCP tool calls) rather than shared tables.

This is deliberate. Premature unification would:
- Force the three tools' data shapes to converge before we know whether convergence helps.
- Couple release cadence (schema migrations in one tool affect all three).
- Compromise the "each tool is independently useful" property — directly violating the Loom federation axiom ([../../suite/loom.md](../../suite/loom.md) §3).

When (or if) unification makes sense, v0.1's design does not foreclose it:
- All three tools speak SQLite; a unified layer could federate across files or migrate to a shared schema.
- The finding-exchange protocol (Filigree's native intake schema) is the canonical cross-tool data shape; a unified store would also speak it.
- Entity IDs are stable strings scoped by tool/plugin namespace; they compose across schemas without rewriting.

For v0.1 through at least v0.3, each tool's store remains its own concern. Revisit when the integration patterns have enough real-world runway to inform the unification design.

---

## Appendix B — Glossary

| Term | Definition |
|---|---|
| **Briefing** | Structured summary answering a fixed set of questions (purpose, maturity, risks, patterns, antipatterns, relationships). Replaces free-form narrative. |
| **Consult mode** | Interactive LLM session using Clarion MCP tools; LLM holds a cursor and navigates the store during the conversation. |
| **Cursor** | Session state pointing at the entity currently under discussion; updated by navigation tools. |
| **Entity** | A node in the property graph — function, class, module, package, subsystem, guidance sheet, file, etc. |
| **Entity briefing** | See Briefing. |
| **Entity ID** | Stable string identifier: `{plugin_id}:{kind}:{canonical_qualified_name}` for source entities. File path not embedded (demoted to a SourceRange property) so the ID survives file moves. See §2. |
| **Edge** | A typed relationship between two entities (`contains`, `calls`, `imports`, `inherits_from`, `decorated_by`, `guides`, `in_subsystem`, ...). |
| **Exploration elimination** | Design principle: every common explore-agent question should be answerable from cache without spawning a sub-agent. |
| **Fact exchange** | Design principle: findings are the primary cross-tool data exchange format; Filigree's native `POST /api/v1/scan-results` intake is the wire format. |
| **Finding** | A structured claim-with-evidence about an entity. Five kinds: Defect, Fact, Classification, Metric, Suggestion. |
| **Guidance** | An entity of `kind: guidance` containing institutional context composed into LLM prompts for related queries. |
| **Guidance fingerprint** | Blake3 hash of the sorted guidance sheet IDs + content hashes applied to a given query; keys the summary cache. |
| **Level** | Policy-config axis: function, class, global, module, subsystem, cross_cutting. |
| **Maturity** | Classification field in EntityBriefing: Placeholder, Experimental, Alpha, Beta, Stable, Mature, Deprecated, Dead. |
| **Observation** | A fire-and-forget note emitted to Filigree with 14-day expiry; can be promoted to an issue or a guidance sheet. |
| **Plugin** | A language-specific subprocess implementing the Clarion plugin protocol; owns its ontology (kinds, edges, tags, rules). |
| **Plugin manifest** | YAML declaration of the kinds, edges, tags, capabilities, rules, and prompt templates a plugin provides. |
| **Policy engine** | Core component deciding for each unit of LLM work: mode, model tier, budget, caching. |
| **Prompt caching** | Anthropic feature layering prompt content into cacheable segments; Clarion structures prompts to maximise hit rate. |
| **Scope lens** | Session filter dictating which relationships neighbour queries emphasise: Structural, Taint, Subsystem, Wardline. |
| **Finding-exchange format** | Filigree's native scan-result intake schema: `{scan_source, scan_run_id?, findings: [{path, rule_id, severity, message, line_start?, line_end?, suggestion?, metadata?}]}`. Accepted at `POST /api/v1/scan-results`. Extension slot is `metadata` (a dict); top-level unknown keys are silently dropped. Used for all cross-tool finding transport inside the suite. Not SARIF. See §7 for the Clarion-specific nesting convention. |
| **SARIF** | Static Analysis Results Interchange Format. Wardline independently emits genuine SARIF v2.1.0 for external consumers (GitHub code-scanning, CI reporters). Not used for suite-internal finding exchange. |
| **Subsystem** | Semantically-grouped cluster of modules, derived by clustering + LLM synthesis. |
| **Summary cache** | Keyed by `(entity, content_hash, template, model_tier, guidance_fingerprint)`; stores generated briefings. |
| **Taint analysis** | Dataflow tracking of tier classifications; **Wardline's responsibility, not Clarion's**. |
| **Tier** | Wardline classification. Canonical names (manifest-configurable in `wardline.yaml:tiers`): `INTEGRAL`, `ASSURED`, `GUARDED`, `EXTERNAL_RAW`. Plus `UNKNOWN` and `MIXED` as computed transient states. Clarion preserves these names verbatim in briefing vocabulary and entity `wardline.declared_tier` property. |
| **Writer-actor** | Concurrency pattern: a single Tokio task owns the sole SQLite write connection; all other tasks submit mutations through a bounded `mpsc` channel. See §3. |
| **Knowledge basis** | EntityBriefing field indicating the evidence class a briefing rests on: `static_only`, `runtime_informed`, or `human_verified`. |
| **Content-Length framing** | JSON-RPC message framing used by the plugin protocol — the same mechanism LSP uses. `Content-Length: <n>\r\n\r\n<json>`. Required for binary-safe streams and crash-resumability. See §1. |
| **Canonical qualified name** | Plugin-language-native fully-qualified identifier used in entity IDs. Python example: `auth.tokens.TokenManager` (with `src/` prefix stripped), not `src/auth/tokens.py::TokenManager`. |
| **Shadow DB** | Operational posture where `clarion analyze` writes to `.clarion/clarion.db.new` and atomic-renames on completion; zero impact on read-snapshot of an already-running `clarion serve`. Available via `clarion analyze --shadow-db`. |
| **Pre-ingest redaction** | Secret-scanner pass (`detect-secrets` or equivalent) executed before any file content reaches the LLM provider; unredacted hits block LLM dispatch for that file. See §8, System-design §10. |
| **Suite bootstrap** | The set of Filigree-side and Wardline-side changes Clarion v0.1 requires as prerequisites. Not "integration with existing capabilities" but "new features in sibling tools." Documented in §9. |
| **`metadata` extension slot** | Filigree's scan-result ingest preserves a per-finding `metadata` dict verbatim. Clarion's extension fields (`kind`, `confidence`, `related_entities`, internal-severity preservation, etc.) nest under `metadata.clarion.*`; Wardline's SARIF property-bag keys nest under `metadata.wardline_properties.*`. Top-level unknown keys are silently dropped by Filigree. |
| **`wardline.fingerprint.json`** | Wardline's per-function state file: `{qualified_name, module, decorators, annotation_hash, tier_context}` per entry. Ingested by Clarion v0.1 into entity `WardlineMeta`. |
| **`REGISTRY_VERSION`** | Semver-ish string on Wardline's `wardline.core.registry` module. Clarion pins it at plugin startup; mismatch emits `CLA-INFRA-WARDLINE-REGISTRY-STALE`. |
| **`scan_run_id`** | Filigree-owned run identifier. Clarion's `run_id` (§2 Finding.source.run_id) maps 1:1; Clarion creates a scan run at Phase 0 and closes it at Phase 8. See §7. |
| **Degraded mode** | Operating posture when a sibling tool is unavailable: `clarion analyze --no-filigree` writes findings locally; `clarion analyze --no-wardline` skips Wardline state ingest. Documented in System-design §11. |
| **Identity reconciliation** | The translation Clarion maintains between three concurrent identity schemes (Clarion EntityId, Wardline qualname, Wardline exception-register location string). See §2. |
| **Entity-resolve endpoint** | `GET /api/v1/entities/resolve?scheme=&value=` — exposes Clarion's identity-translation layer as a public API so sibling tools can look up Clarion entity IDs from their native schemes without embedding Clarion's ID format. See §7. |
| **Capability probe / compat report** | At `clarion analyze` startup, Clarion probes Filigree and Wardline capabilities and emits one `CLA-INFRA-SUITE-COMPAT-REPORT` finding summarising what's present and what's degraded. See System-design §11. |
| **Textual DB export** | `clarion db export --textual` — deterministic JSON-lines dump of entities, edges, guidance, findings (summary cache excluded). Enables git-friendly diffs and multi-developer merge resolution on the committed SQLite database. See §3. |
| **DB merge-helper** | `clarion db merge-helper` — Git merge driver that resolves `.clarion/clarion.db` conflicts deterministically: union entities/edges, last-writer-wins by `updated_at`, guidance conflicts surfaced for manual resolution, cache cleared. See §3. |
| **Triage-state feedback** | Mechanism by which Filigree's suppression and acknowledgement reasons surface inside Clarion briefings as operator-acknowledged evidence or synthetic risk entries. Read-only on Clarion's side. |
| **`first_seen_commit` / `last_seen_commit`** | Entity-level provenance: the git SHA of the first run to observe an entity and of the most recent run to still observe it. Enables point-in-time queries without re-running analysis. |

---

## Appendix C — Rust stack

The v0.1 core is Rust (locked — see §11 ADR-001). Crate choices below are recommendations informed by the design review; they belong in the implementation plan as pinned versions but are named here so downstream work doesn't re-derive them.

### Async runtime

- **tokio** — effectively required by the chosen axum/reqwest/sqlx ecosystem; nothing else in the ecosystem is competitive for this workload.
- Work-stealing runtime (`#[tokio::main(flavor = "multi_thread")]`) except in tests, where `current_thread` avoids nondeterminism in concurrency assertions.

### HTTP

- **axum** for the read API (§7): integrates cleanly with tokio and `tower` middleware; Bearer-token auth, metrics, and ETag middleware compose cleanly.
- **reqwest** for Anthropic API calls; pooled connections; TLS via `rustls` (no native-TLS; see below).

### SQLite

- **rusqlite** + `features = ["bundled", "serde_json", "blob", "functions", "limits", "json"]`.
- **deadpool-sqlite** for the read-connection pool (default max 16).
- **One** writable `rusqlite::Connection` inside the writer actor (§3 Concurrency). Do not pool writers.
- Preferred over `sqlx`'s `sqlite` feature: this workload is prepared-statement heavy with FTS5 and JSON1, and `sqlx`'s type-checked query-macros pay a compilation cost for minimal benefit here.
- `rusqlite_migration` for the numbered-migration feature promised in §3.

### TLS

- **rustls** + **webpki-roots** — pure-Rust TLS stack. Preserves the single-binary portability claim. Native-TLS would require OpenSSL at runtime on mixed Linux distributions and break portability; explicitly not used.

### Plugin protocol

- Content-Length framed JSON-RPC 2.0 (§1). Implementation: `tower-lsp-server`-derived framing or hand-rolled (simple enough — ~80 lines). Use `serde_json` for payload encoding; `jsonrpc-core` is over-featured for our two-endpoint protocol.
- **tokio::process::Child** for plugin subprocess lifecycle. Explicit `wait()` to reap zombies. SIGPIPE handling (Unix only): ignore the signal so a dead plugin doesn't crash the core when we write to its stdin.
- **Bounded `tokio::sync::mpsc`** for the `file_analyzed` stream from plugin → core. Backpressure cap: default 100 messages. Prevents a runaway plugin from OOMing the core.
- **Crash-loop circuit breaker**: >3 plugin crashes in 60 seconds → permanently disable the plugin for the run and emit `CLA-INFRA-PLUGIN-DISABLED-CRASH-LOOP`.
- **stdout hygiene**: documented plugin-author requirement (§1 Error handling). Reference Python client redirects `logging.basicConfig(stream=sys.stderr)` at import time.

### Observability

- **tracing** + **tracing-subscriber** for structured JSON-lines logs. `tracing_log::LogTracer` for `log`-crate interop.
- **metrics** + **metrics-exporter-prometheus** for `/api/v1/metrics`.
- Log rotation: **tracing-appender** with rolling-file 100MB × 5 files.

### Graph algorithms

- In-memory graph projections feed the Leiden adapter and local weighted-components fallback. ADR-006 captures the Leiden decision; ADR-032 names the local fallback.

### Hashing, IDs, time

- **blake3** for all content hashes (fingerprints, entity IDs where applicable, content-addressed paths).
- **uuid** v7 for run IDs (time-sortable, useful for log scanning).
- **time** (not `chrono`) for all datetime handling — strict, explicit, and serde-friendly.

### Cryptography

- **rand** + **rand_core** for token generation.
- **subtle** for constant-time comparison of auth tokens.
- **keyring** crate for OS keychain (§7); `0600` file fallback uses `std::os::unix::fs::PermissionsExt`.

### Testing

- **insta** for snapshot testing (briefing JSON structures, markdown renderer output).
- **proptest** for property-based tests (§10) — graph invariants, ID stability across file moves.
- **wiremock** or **mockito** for HTTP tests.
- **tempfile** for test fixtures.

### Build

- **Cargo workspaces**: `clarion-core`, `clarion-cli`, `clarion-plugin-protocol`, `clarion-api`, `clarion-llm`.
- Cross-compilation targets per §10 Operational: Linux x86_64/ARM64, macOS x86_64/ARM64, Windows x86_64. Use `cargo-dist` or similar for release artifacts.
- MSRV: 1.75+ (stable async traits). Document in README.

### Schema specifics (complement to §3)

- `UNIQUE(kind, from_id, to_id)` on `edges` — prevents duplicate edges from plugin retry or ambiguous AST matches.
- FTS5 triggers wiring `entities` → `entity_fts` — required for mutations to stay searchable; see §3 schema block.
- Generated columns + indices on `scope_level`/`scope_rank` and `git_churn_count` — hot-path filters avoid `json_extract()` in WHERE clauses. The `scope_rank` integer column carries a CASE-mapped rank (1..6) so `ORDER BY scope_rank` produces the documented composition order; `scope_level` keeps the human-readable enum value for equality filtering. See ADR-024.
- `PRAGMA foreign_keys = ON` every connection (rusqlite does not enable by default).

---

## Appendix D — Revision history

Revision 5 (2026-04-17) restructures the single design document into a three-layer docset. Revision 4 addressed a follow-up review focused on feedback loops, degraded-mode combinations, and operator-facing sharp edges. Revision 3 addressed the integration reconnaissance. Revision 2 addressed the design review. All five are indexed below, Rev 5 first.

### Rev 5 changes (docs restructure, 2026-04-17)

| Change | Rationale |
|---|---|
| Original document split into three layers: requirements (the *what*), system-design (the *how*, mid-level), and this detailed-design reference (implementation-level) | Original doc reached 71k tokens and mixed high-level intent with implementation detail; the three-layer docset lets a reader choose the appropriate depth without reading everything |
| Filename renamed `clarion-v0.1-design.md` → `clarion-v0.1-detailed-design.md` via `git mv` (history preserved via `git log --follow`) | Removes ambiguity between "the design doc" and the new `system-design.md` |
| Abstract, Design Principles, process/UX topology, conceptual data model, core/plugin split narrative, integration posture, security threat model, suite-bootstrap architecture, §12 Explicit Deferrals, and §1 "What Clarion is NOT" moved to higher layers (requirements or system-design) | Higher layers serve those abstractions better; this document focuses on implementation detail |
| Retained: full SQL schema, complete `clarion.yaml` example, exact Phase-7 rule catalogue with thresholds, full MCP tool list, severity mapping tables, `scan_run_id` lifecycle, dedup collision policy, commit-ref/dirty-tree handling, SARIF translator detail, Wardline state-file table, HTTP endpoint list, token-auth full spec, operator-guidance security subsection, §11 Suite Bootstrap prerequisite detail, testing/observability/acceptance, full ADR backlog, appendices | These are the implementation details that don't belong in higher layers |
| Suite naming aligned: "three-tool suite" framing replaced by "Loom suite" / "Loom fabric" references, with `docs/suite/loom.md` as the canonical doctrine source | Loom is now the family name; see [../../suite/loom.md](../../suite/loom.md) for federation axiom and composition law |
| Section numbering renumbered 1-11 (not gapped) to match the document's reduced scope | Readers don't encounter gaps in numbering; cleaner navigation |

### Rev 4 changes (from follow-up review, 2026-04-17)

| Feedback item | Action taken | Now in |
|---|---|---|
| Gap 1: Triage state has no path back into briefings | Read-only feedback loop in briefing composition; operator-acknowledged evidence / synthetic risk entries; `guidance_fingerprint` includes acknowledged finding IDs | System-design §7 Composition, System-design §3 EntityBriefing |
| Gap 2: Degraded-mode matrix incomplete (combinations & version skew) | Capability-negotiation probe at `clarion analyze` startup; single `CLA-INFRA-SUITE-COMPAT-REPORT` finding summarising all probe results; per-component fallback table covers pre-flag Filigree, REGISTRY additive skew, SARIF property-bag removal | System-design §11 |
| Gap 3: SQLite merge conflicts on committed `.clarion/clarion.db` | Promoted `clarion db export --textual` to v0.1; added `clarion db merge-helper` with git-merge-driver integration | §3 |
| Gap 4: No story for entity deletion between runs | `CLA-FACT-ENTITY-DELETED` emitted at Phase 7 via entity-set diff; `CLA-FACT-GUIDANCE-ORPHAN` for affected sheets; cache invalidation on deletion | §5, System-design §6 |
| Leverage 1: Clarion as cross-tool identity oracle | New `GET /api/v1/entities/resolve?scheme=&value=` endpoint; covers wardline_qualname, wardline_exception_location, file_path, sarif_logical_location | §7 |
| Leverage 2: Subsystem-tier-mixing rule | `CLA-FACT-TIER-SUBSYSTEM-MIXING` and `CLA-FACT-SUBSYSTEM-TIER-UNANIMOUS` added to Phase 7; unique-to-Clarion structural signal | §5 |
| Leverage 3: `knowledge_basis` has no promotion path | Added promotion rule: guidance authorship matching an entity, or acknowledged/suppressed finding with reason, promotes briefing to `HumanVerified` | §2 EntityBriefing |
| Leverage 4: Commit SHAs on runs but not on entities | Added `first_seen_commit` / `last_seen_commit` to Entity struct and schema; indexed; dirty-tree runs use underlying SHA | §2, §3 |
| Small: SARIF translator framing | Reframed as a permanent general-purpose feature (Semgrep, CodeQL, etc.); only Wardline-specific ownership moves in v0.2 | §7 |
| Small: TTL + pinned-sheet interaction | Churn-triggered eager invalidation for guidance-stale entities; `briefing_guidance_may_be_stale` flag on responses | §4 |
| Small: Operator API-key risk | §8 Operator guidance subsection; threat-model row for personal-key commits | §8 |

### Rev 3 changes (from integration recon, 2026-04-17)

| Recon item | Action taken | Now in |
|---|---|---|
| §1 headline: suite fabric does not exist yet | Reframed v0.1 scope as "weaving the Loom fabric"; Abstract rewritten; new §11 Suite Bootstrap | System-design, §9 |
| §3.1 Wire format wrong — `properties`→`metadata`, `line`→`line_start/line_end` | Finding struct and wire-format block rewritten; full example JSON emitted | §2, §7 |
| §3.2 Severity enum wrong — wire values `{critical,high,medium,low,info}` | Mapping table added; internal vocabulary preserved in `metadata.clarion.internal_severity`; `warnings[]` response inspection required | §7 |
| §3.3 Wardline groups 9/12/13 are decorator-based (Rev 2 fix was wrong) | All 17 groups listed under `wardline_groups`; `wardline_overlay_groups: [1, 17]` notes supplementary overlay declarations | §1 manifest |
| §3.4 Wardline tier vocabulary uses `INTEGRAL/ASSURED/GUARDED/EXTERNAL_RAW`, not T1–T4 | Glossary and WardlineMeta struct corrected; `declared_tier: Option<TierName>` | §2, Appendix B |
| §3.5 Wardline has no Filigree integration today | SARIF→Filigree translator (Clarion-side `clarion sarif import`) specified in §7; §9.2 documents Wardline prerequisite | §7, §9.2 |
| §3.6 `registry_backend` flag does not exist in Filigree | ADR-014 added; §9.1 names the schema surgery (4 NOT-NULL FKs, 3 auto-create paths, 5–8 hot files); degraded-mode fallback in System-design §11 | §7, §9.1, ADR-014 |
| §3.7 Observation API is MCP-only in Filigree | ADR-016 added; §9.1 asks Filigree for `POST /api/v1/observations`; Clarion workaround (MCP client) documented | §9.1 |
| §3.8 HTTP routes parallel displaced MCP tools | §7 addresses: pick redirect-to-Clarion or shadow-read; cannot mix | §7, ADR-014 |
| §3.9 `get_issue` default-embeds file IDs | Documented as user-visible leak in §7 release-notes requirement | §7 |
| §3.10 `_schema` does not enumerate `scan_source` | Glossary and §7 corrected; `scan_source` is social convention | §7, Appendix B |
| §4.1 Wardline SARIF property-bag translation layer missing | Translator design added; `metadata.wardline_properties.*` nesting convention | §7 |
| §4.2 Severity/rule-ID mapping tables missing | Mapping table + rule-ID round-trip policy added | §7 |
| §4.3 `wardline.fingerprint.json` and other state files not ingested | State-file in/out table (10 files) added; fingerprint + exceptions in v0.1 scope | §7 Wardline |
| §4.4 `REGISTRY_VERSION` compatibility mechanism missing | Direct import with version pin; `CLA-INFRA-WARDLINE-REGISTRY-STALE` finding | §1, §9.2, ADR-018 |
| §4.5 Legacy decorator aliases unhandled | Row added to decorator-detection table | §1 Python plugin specifics |
| §4.6 Class decoration goes beyond Wardline | `confidence_basis: "clarion_augmentation"` tag added | §1 |
| §4.7 Three auto-create paths in Filigree | Named explicitly in §9.1 under registry_backend | §9.1 |
| §4.8 Observation transport ambiguity | ADR-016 resolves: HTTP endpoint in Filigree (primary); MCP client fallback | §9.1, ADR-016 |
| §4.9 Scanner registration for Filigree dashboard | Deferred to v0.2; explicit non-silence | Requirements NG-* |
| §4.10 `scan_run_id` lifecycle | Mapped 1:1 to Clarion `run_id`; Phase 0 creates, Phase 8 completes | §7 |
| §4.11 Dedup key collision on entity moves | `mark_unseen=true` + `--prune-unseen` policy; v0.2 server-side dedup deferral | §7, Requirements NG-21 |
| §4.12 Integration test fixtures missing | §10 Acceptance Ecosystem expanded to 8 criteria; mock Filigree, SARIF corpus, pin tests, end-to-end smoke | §10 |
| §4.13 Commit-ref dirty handling silent | Policy added: `-dirty` suffix recorded; `--require-clean` flag | §7 |
| §4.14 BAR subsystem awareness | Non-goal note deferred to v0.2 framing | Requirements NG-19 |
| §4.15 Identity scheme alignment | New §2 Identity reconciliation subsection; translation-layer posture | §2 |
| §4.16 Filigree CHANGELOG breaking-change posture | `_schema` pin test added as acceptance criterion | §10 |

### Rev 2 changes (from design review, 2026-04-17)

| Review item | Action taken | Now in |
|---|---|---|
| §1.1 SARIF-lite misframing | Adopted Filigree-native intake as canonical format; SARIF framing removed | System-design Abstract, §2, §7, Appendix B |
| §1.2 Model IDs | Pinned against Anthropic SDK with CI-guard note | §4 providers block |
| §1.3 Wardline groups 9/12/13 | Superseded by Rev 3 correction — see above | §1 manifest |
| §1.4 LSP framing ambiguity | Content-Length framing adopted explicitly | §1 |
| §2.1 Entity ID stability (CRITICAL) | Symbolic canonical-name IDs; file path demoted to property; manual `--repair-aliases` v0.1 | §2 Entity ID scheme |
| §2.2 SQLite concurrency (CRITICAL) | Writer-actor model; per-N-files transactions; shadow-DB option | §3 Concurrency |
| §2.3 Python import resolution (CRITICAL) | Policies for sys.path, unresolved imports, re-exports, conditional imports | §1 Python plugin specifics |
| §2.4 Secrets + prompt injection (CRITICAL) | Pre-ingest redaction; injection containment; operator guidance | §8, System-design §10 |
| §2.5 Provider abstraction (HIGH) | Honest framing added — v0.1 is Anthropic-shaped by commitment | §4 LLM provider abstraction (System-design §5) |
| §2.6 HTTP auth (HIGH) | Token auth designed in v0.1; OS keychain; Wardline pickup path | §7 HTTP read API |
| §3 ADR backlog | §11 with priorities; ADR-001 (Rust) locked without alternatives | §11 |
| §4.1 Summary cache semantic validity | TTL backstop + graph-neighborhood drift flag | §4 |
| §4.2 Guidance stock quality | Churn-tied staleness signals | System-design §7 |
| §4.3 Finding stock drain | v0.2 triage-feedback loop added | Requirements NG-17 |
| §4.4 LLM capability atrophy | `knowledge_basis` field on briefings | §2 EntityBriefing |
| §5 Python plugin gaps | Parser dispatch, decorator policy, TYPE_CHECKING, packaging, serial v0.1 | §1 Python plugin specifics |
| §6 Rust stack addendum | Appendix C | Appendix C |
| §8 Observe-vs-enforce leak | v0.2 annotation descriptor (Python-direct import in v0.1, YAML descriptor in v0.2) | §1 Observe-vs-enforce, Requirements NG-25 |

### Rev 3 meta-note

The Rev 2 → Rev 3 transition is the classic layered-review problem. Rev 2 corrected against a design review that read the codebase *interpretively*; Rev 3 corrects against a reality check that read the codebase *directly*. Rev 2's SARIF-lite → Filigree-native correction was the right direction but imprecise on field names. Rev 2's Wardline-groups split was responsive to the review's phrasing but factually wrong. Future design reviews in this suite should run reality-check passes as a default step before landing design edits.

---

**End of Clarion v0.1 detailed design reference.**
