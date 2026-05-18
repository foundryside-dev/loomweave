# Clarion v0.1 Integration Reconnaissance

**Original reviewed document (pre-restructure)**: `2026-04-17-clarion-v0.1-design.md` (Revision 2)
**Historical note**: this reconnaissance evaluates the pre-restructure single-file design at revision 2. The current canonical Clarion v0.1 docset lives in [../README.md](../README.md). This file is retained as supporting context, not as normative guidance.
**Companion review**: [design-review.md](./design-review.md)
**Recon date**: 2026-04-17
**Method**: Read-only static analysis of `/home/john/filigree` and `/home/john/wardline`, run in five parallel focused sweeps (scan-results intake, file registry, Wardline annotations, Wardline SARIF & entity model, cross-tool flows). Every claim in this document is file:line-cited or declared as an absence-verified negative.
**Verdict**: **Design revision is required before the implementation plan.** Two assertions are factually wrong (finding-field names, Wardline group 9/12/13 declaration path), one major architectural premise is unsupported by code today (Wardline as an HTTP consumer of Clarion), and the suite's cross-tool fabric is mostly aspirational — Clarion v0.1 will be *creating* the integration surface, not *joining* it.

---

## 1. Executive summary

Five findings change the Clarion design's integration posture the most:

1. **Clarion's finding wire format as described will silently drop its richest data.** Filigree's per-finding extension slot is `metadata`, not `properties`; its line field is split as `line_start`/`line_end`, not `line`; its severity vocabulary is `{critical,high,medium,low,info}`, not `{INFO,WARN,ERROR,CRITICAL}`. `WARN` and `ERROR` are coerced to `info` with a warning in the response. Every `kind`, `confidence`, `confidence_basis`, `supports`, `supported_by`, `related_entities` field Clarion wants to emit disappears silently unless nested inside `metadata`. (§2.1, §3.1)

2. **Wardline groups 9, 12, 13 are decorator-declared, not manifest-declared.** All 17 Wardline groups are decorator-based; the design's Revision-2 claim that these three are "declared separately in `wardline.yaml`" is wrong. Wardline already ships the canonical descriptor as `wardline.core.registry.REGISTRY` — a frozen Python `MappingProxyType` over 42 canonical decorator names plus 1 legacy alias, with a `REGISTRY_VERSION` string. The v0.2 "Wardline annotation descriptor" inversion the design defers is a YAML-dump of an existing data structure, not net-new architecture. (§2.3, §3.3)

3. **The suite's cross-tool fabric does not exist in code yet.** Wardline has zero HTTP client code in its scanner path (`grep` for `requests|urllib|httpx|aiohttp|HTTPConnection` across `src/wardline` returns zero hits; `pyproject.toml:22` declares `dependencies = []`). Wardline does not post to Filigree today. Filigree has no `wardline` references anywhere in its source. No cross-tool fixtures, mocks, contract tests, or shared schemas exist. The design's "Wardline findings flow to Filigree via Wardline's own integration; Clarion doesn't mediate" is false today: Clarion will *install* that fabric, or Wardline ships a bridge, or the findings don't flow. (§2.5, §2.6, §2.7)

4. **Filigree's file-registry displacement is not a config-flag problem, it is schema surgery.** The `registry_backend: filigree|clarion` flag and `FILIGREE_FILE_REGISTRY_DISPLACED` error code exist only in the Clarion design — zero matches in Filigree's codebase. `file_id` is already a string, but four tables (`scan_findings`, `file_associations`, `file_events`, `observations`) hold NOT-NULL foreign keys to `file_records(id)`. Three auto-create paths (`POST /api/v1/scan-results`, `observe(file_path=…)`, `trigger_scan`) insert `file_records` rows that the design's displacement story does not touch. The HTTP-route surface (11 routes in `dashboard_routes/files.py`) parallels the MCP tools the design displaces but is not mentioned. (§2.2, §3.2)

5. **The design underspecifies what Clarion ingests from Wardline.** Wardline's authoritative per-function state lives in `wardline.fingerprint.json` (annotation_hash, decorators, tier context), not `wardline.yaml`. The overlay schema already declares `boundaries[]` for Groups 1 and 17. A further six project-state JSON files (`wardline.exceptions.json`, `wardline.compliance.json`, `wardline.conformance.json`, `wardline.perimeter.baseline.json`, `wardline.manifest.baseline.json`, `wardline.retrospective-required.json`) carry real Wardline state the design does not name. §2 of the design proposes ingesting `wardline.yaml + overlays` only; that is a partial view. (§2.4, §4.4)

The pattern across these five findings: the design reads adjacent-tool behavior optimistically. Recon shows ground truth is less finished and less uniform. Two corrections are literal edits; three are design decisions that need to be made explicit before implementation.

---

## 2. Per-question answers

### 2.1 Filigree's scan-results intake shape (Question 1)

**Location confirmed.** Handler decorator at `/home/john/filigree/src/filigree/dashboard_routes/files.py:294`; function body `api_scan_results` at `:295-330`. Mounted at `/api/v1/scan-results` via router prefix in `dashboard.py:320,343`. Also mounted at `/api/p/{project_key}/v1/scan-results` for project-scoped posts.

**Request body (top-level)**:

| Field | Type | Required | Default |
|---|---|---|---|
| `scan_source` | non-empty string | yes | — |
| `findings` | JSON array | yes | — |
| `scan_run_id` | string | no | `""` |
| `mark_unseen` | bool | no | `false` |
| `create_observations` | bool | no | `false` |
| `complete_scan_run` | bool | no | `true` |

Validation at `files.py:300-317` is hand-rolled `isinstance` + empty-string checks; **no pydantic, no jsonschema**. Reference: `db_files.py:386-428` (`_validate_scan_findings`).

**Per-finding fields** (`db_files.py:386-428`, `_upsert_finding` at `:455-556`):

| Field | Type | Required | Notes |
|---|---|---|---|
| `path` | non-empty string | yes | Normalised via `_normalize_scan_path`: `\→/`, `os.path.normpath`. POSIX relative paths preferred. |
| `rule_id` | non-empty string | yes | Free-form; no enum. |
| `message` | non-empty string | yes | — |
| `severity` | string | no, default `"info"` | Enum `{critical,high,medium,low,info}` (`types/core.py:14`, enforced by DB `CHECK` at `db_schema.py:147`). Unknown values **coerced to `"info"` with a warning in the response** (`db_files.py:411-427`), not rejected. |
| `line_start` | int\|null | no | — |
| `line_end` | int\|null | no | — |
| `suggestion` | string | no | Truncated at 10,000 chars (`db_files.py:474-482`). |
| `language` | string | no | Propagated onto file record. |
| `metadata` | dict | no | **The only extension slot.** Serialised as JSON and preserved verbatim. |

Any top-level finding key *outside* this enumerated set is **silently dropped** (no `**kwargs` capture; `db_files.py:501-525` reads each key by name). Keys *inside* `metadata` round-trip (verified by `tests/core/test_files.py:496-540`, test `test_scan_metadata_persisted_on_create`).

**Dedup key** (`db_schema.py:156-157`): `UNIQUE(file_id, scan_source, rule_id, coalesce(line_start, -1))`. Re-posts with the same 4-tuple overwrite; second column/qualname at the same line is lost.

**Error response** (`dashboard_routes/common.py:46-51`): `{"error": {"message": "...", "code": "VALIDATION_ERROR", "details": {}}}`, HTTP 400.

**Success response** (`types/files.py:138-148`, `ScanIngestResult`): `{files_created, files_updated, findings_created, findings_updated, new_finding_ids, observations_created, observations_failed, warnings}`. Note `warnings` — Clarion must read this, not just the count, to detect severity coercion.

**`scan_source` is free-form**. No enum, no registry. `POST .../scan-results` with `scan_source="anything"` is accepted. Existing in-tree scanners use `"codex"`, `"claude"`, `"claude-code"`, `"scanner"`, `"test-scanner"` (evidence: `tests/core/test_scans.py`).

**`GET /api/files/_schema`** at `files.py:94-175` returns:

```
valid_severities          (5 values, sorted)
valid_finding_statuses    (5 values: open, acknowledged, fixed, false_positive, unseen_in_latest)
valid_association_types   (4 values: bug_in, mentioned_in, scan_finding, task_for)
valid_file_sort_fields
valid_finding_sort_fields
endpoints                 (12 entries, each with method/path/description/status;
                           only /api/v1/scan-results has inline request_body shape)
```

Hardcoded dict literal (not pydantic-derived). Cache: `max-age=3600`. **Does NOT enumerate `scan_source` values** (there is no such list). The design's claim that `_schema` is "source of truth for enums including valid `scan_source` identifiers" is partly wrong: it is source of truth for severity, status, and association_type; `scan_source` is unvalidated server-side.

**Existing callers** (`/home/john/filigree` repo): `scripts/scan_utils.py:post_to_api` at `:312-382` is the canonical client. Called from `scripts/claude_bug_hunt.py`, `scripts/codex_bug_hunt.py`. Tests drive the endpoint with `httpx.AsyncClient`. **No Wardline caller in-tree** — `grep -r wardline` across Filigree returns zero matches.

### 2.2 File-registry displacement impact (Question 2)

**MCP tool inventory** (all in `/home/john/filigree/src/filigree/mcp_tools/files.py`):

| Tool | Tool def | Handler | Tables touched |
|---|---|---|---|
| `register_file` | `:119-132` | `_handle_register_file:387-422` | `file_records`, `file_events` |
| `list_files` | `:36-61` | `_handle_list_files:245-289` | `file_records`, `scan_findings`, `file_associations`, `observations` |
| `get_file` | `:62-72` | `_handle_get_file:292-304` | `file_records`, `file_associations`, `scan_findings`, `observations` |
| `get_file_timeline` | `:73-90` | `_handle_get_file_timeline:307-336` | `scan_findings`, `file_associations`, `file_events`, `issues` |
| `add_file_association` | `:102-118` | `_handle_add_file_association:354-384` | `file_associations`; reads `file_records`, `issues` |
| `get_issue_files` | `:91-101` | `_handle_get_issue_files:339-351` | `file_associations` JOIN `file_records` JOIN `issues` |

**`file_id` is already an opaque TEXT primary key.** `db_schema.py:117` declares `id TEXT PRIMARY KEY`. Generation (`db_issues.py:178-198`): `f"{prefix}-f-{uuid.uuid4().hex[:10]}"`. Project-configurable prefix via `.filigree/config.json`. **String-for-string substitution with Clarion entity IDs is feasible at the type level.**

**Four NOT-NULL foreign keys point at `file_records(id)`** (`db_schema.py`):

1. `scan_findings.file_id` — `:131`
2. `file_associations.file_id` — `:161`
3. `file_events.file_id` — `:198`
4. `observations.file_id` — nullable, `ON DELETE SET NULL` — `:214`

**Three auto-create paths the design does not address**:

1. **`POST /api/v1/scan-results`** — calls `_upsert_file_record` at `db_files.py:430-453` before inserting findings. If Clarion keeps emitting findings via HTTP (which it plans to), every POST creates a shadow `file_records` row under the Filigree-native identity scheme, not Clarion's scheme. This contradicts "Clarion owns the file registry."
2. **`create_observation(file_path=…)`** — `db_observations.py:135-147` calls `register_file` to bind the observation. Clarion's plan to emit observations to Filigree auto-populates Filigree's registry.
3. **`trigger_scan` / `trigger_scan_batch`** — `mcp_tools/scanners.py:422` and `:586` call `tracker.register_file(...)` to populate `scan_runs.file_ids`.

**`get_issue` default-embeds `get_issue_files`** (`mcp_tools/issues.py:368-370`, `include_files=True` by default). Under Clarion mode, every `get_issue` call surfaces "retained but opaque" `file_id`s that agents reading the default response see unexpectedly. The design's "retained" framing understates the user-visible leak.

**HTTP route surface parallels MCP tools**. `dashboard_routes/files.py` has 11 routes (`GET /files`, `GET /files/{file_id}`, `GET /files/{file_id}/findings`, `GET /files/{file_id}/timeline`, `POST /files/{file_id}/associations`, `GET /files/hotspots`, `GET /files/stats`, `GET /scan-runs`, `GET /files/_schema`, `PATCH /files/{file_id}/findings/{finding_id}`, `POST /v1/scan-results`). The design says MCP tools are removed for Clarion projects but is silent on the HTTP routes that reach the same data. The dashboard UI uses HTTP.

**`registry_backend` flag and `FILIGREE_FILE_REGISTRY_DISPLACED` error code do not exist**. `grep` across `/home/john/filigree` returns zero matches for each. Both are proposed net-new additions.

**Surgery estimate for pluggable registry**: ~5–8 hot files (`db_files.py`, `mcp_tools/files.py`, `mcp_tools/scanners.py`, `mcp_tools/issues.py`, `dashboard_routes/files.py`, `dashboard_routes/issues.py`, `db_observations.py`, plus `db_schema.py`/`migrations.py` for FK rework). ~17 test files reference `file_id` directly (hundreds of call sites). There is no `FileRegistryProtocol` today; `FilesMixin` is composed into the monolithic `FiligreeDB` class via MRO (`core.py:344`). The design's "config flag" framing underbills the work.

**Zero existing cross-project callers**. Neither Wardline nor any Clarion code calls the displaced MCP tools. Blast radius is entirely Filigree-internal (tests, dashboard, `get_issue` default-embed, scanner trigger's `scan_runs.file_ids` column).

**JSONL migration substrate exists**. `db_meta.py:524-536` exports/imports `file_record`, `scan_finding`, `file_association`, `file_event` record types. `_resolve_imported_file_id` at `db_meta.py:466-513` handles ID/path conflict resolution. Clarion's backfill command (`clarion migrate filigree-files`) has a natural entry point here.

### 2.3 Wardline's real annotation vocabulary (Question 3)

**Groups are a formal concept.** Integer field on `RegistryEntry` at `/home/john/wardline/src/wardline/core/registry.py:30`; validated at decorator construction at `_base.py:79-84`; normative in `docs/spec/wardline-01-07-annotation-vocabulary.md:13-32`. 17 groups total.

**The design's Revision-2 claim that groups 9, 12, 13 are "declared separately in `wardline.yaml`" is WRONG.** All three have decorators, all three are enforced by `scanner/rules/sup_001.py` reading decorator-derived `WardlineAnnotation` records (`sup_001.py:303, 322-325`). Manifest (`wardline.schema.json`) and overlay (`overlay.schema.json`) schemas have no fields for these groups. Corpus specimens: `corpus/specimens/SUP-001/EXTERNAL_RAW/negative/SUP-001-TN-atomic.py:1-5` uses `@atomic`; `SUP-001-TN-not-reentrant.py:1-5` uses `@not_reentrant`.

**Full decorator inventory: 42 canonical names + 1 legacy alias.** Single source: `core/registry.py:55-237` (`REGISTRY: MappingProxyType[str, RegistryEntry]`). Abbreviated table (full table in source file, columns: canonical_name, group, factory?, file:line):

- **Group 1 (Authority Tier Flow, 7)**: `external_boundary`, `validates_shape`, `validates_semantic`, `validates_external`, `integral_read`, `integral_writer`, `integral_construction` — all bare, `authority.py:22-65`
- **Group 2 (Integrity Primacy, 1)**: `integrity_critical` — bare, `integrity.py:15`
- **Group 3 (Plugin Contract, 1)**: `system_plugin` — bare, `plugin.py:14`
- **Group 4 (Data Provenance, 1)**: `int_data` — bare, `provenance.py:15`
- **Group 5 (Schema Contracts, 2 + 1 call-site marker)**: `all_fields_mapped` (dual-form), `output_schema` (bare in code, factory in spec — mismatch), `schema_default()` helper — `schema.py:33,53,24`
- **Group 6 (Layer Boundaries, 1)**: `layer` — factory, `boundaries.py:32`
- **Group 7 (Template/Parse Safety, 1)**: `parse_at_init` — bare, `safety.py:11`
- **Group 8 (Secret Handling, 1)**: `handles_secrets` — bare, `secrets.py:11`
- **Group 9 (Operation Semantics, 3)**: `idempotent`, `atomic`, `compensatable` (factory) — `operations.py:21,24,28`
- **Group 10 (Failure Mode, 6)**: `fail_closed`, `fail_open`, `emits_or_explains`, `exception_boundary`, `must_propagate`, `preserve_cause` — `operations.py:37-72`
- **Group 11 (Data Sensitivity, 3)**: `handles_pii`, `handles_classified`, `declassifies` — all factory, `sensitivity.py:16-36`
- **Group 12 (Determinism, 2)**: `deterministic`, `time_dependent` — `determinism.py:12,19`
- **Group 13 (Concurrency, 3)**: `thread_safe`, `ordered_after` (factory), `not_reentrant` — `concurrency.py:13-30`
- **Group 14 (Access/Attribution, 2)**: `requires_identity`, `privileged_operation` — `access.py:12,19`
- **Group 15 (Lifecycle/Scope, 3)**: `test_only`, `deprecated_by` (factory), `feature_gated` (factory) — `lifecycle.py:15-30`
- **Group 16 (Generic Trust Boundary, 2 + 1 alias)**: `trust_boundary` (dual-form), `data_flow` (factory), `tier_transition` (legacy alias → `trust_boundary`) — `boundaries.py:74,135,108`
- **Group 17 (Restoration Boundaries, 1)**: `restoration_boundary` — factory, `restoration.py:30`

**Overlay schema declares `boundaries[]` for Groups 1 and 17** (`overlay.schema.json:16-168`). These supplement source-level decorators for those two groups — a manifest-declaration path the design partly captures under "Clarion ingests `wardline.yaml` + overlays" but does not tie to specific groups.

**Existing descriptor file**: **Yes — `core/registry.py:REGISTRY` is the catalog.** Pure data (`MappingProxyType` with frozen `RegistryEntry` rows). `REGISTRY_VERSION` string at `registry.py:10`. No YAML export exists today, but dumping `REGISTRY` to YAML is trivial. The v0.2 "annotation descriptor" inversion is an export step, not net-new architecture.

**Wardline's own scanner does NOT handle class decorators** — `scanner/discovery.py:_walk_functions` only visits `FunctionDef`/`AsyncFunctionDef`. Clarion's design promises "Class decorator (`@register\nclass C:`) — Recorded identically to function decorators", which is **more permissive than Wardline itself**. Clarion should clarify that class-decoration data beyond what Wardline emits is Clarion-side augmentation, not Wardline-authoritative.

**Detection cases Wardline handles**: direct named, factory, stacked, aliased (`alias.asname`), dotted single-level (`@wardline.x`). **Does NOT handle**: arbitrary dotted chains (`a.b.c`), star imports (refuses with warning at `discovery.py:157-159`), dynamic imports (warns at `:215-274`), metaclass-based decoration, lambdas/subscripts as decorators. Clarion's policy table should state it matches or exceeds Wardline's handling, and name the non-handled cases as shared blind spots.

**`tier_transition` is a legacy alias of `trust_boundary`** via `LEGACY_DECORATOR_ALIASES` (`registry.py:12-14`). Clarion keyed only on source-level name misses this identity. The `LEGACY_DECORATOR_ALIASES` table is the sanctioned extension point.

### 2.4 Wardline's SARIF emitter (Question 4)

**Emitter**: `/home/john/wardline/src/wardline/scanner/sarif.py` (entire file). `SarifReport.to_dict` at `:440-558`. Called from `cli/scan.py:1144-1192`.

**Shape**: genuine SARIF v2.1.0. Schema at `sarif.py:25-28`. Version at `:557`. `runs[].tool.driver` has `name="wardline"`, `informationUri`, `rules`, `version` (overridden at runtime from `_wardline_pkg.__version__`). **`semanticVersion` and `guid` are NOT emitted.** `runs[].results[].locations[].physicalLocation.artifactLocation.uri` is POSIX-relative via `_normalize_artifact_uri` (`:233-246`). `region.startLine/startColumn/endLine/endColumn/snippet` via `_make_region` at `:218-230`.

**Property-bag extensions (load-bearing!)**. Wardline puts domain semantics into SARIF's `properties` bag, not into first-class SARIF fields. Run-level: **35 `wardline.*` keys** (examples: `wardline.controlLaw`, `wardline.governanceProfile`, `wardline.propertyBagVersion="0.8"`, `wardline.commitRef`, `wardline.coverageRatio`, …). Result-level: **9 keys** (`wardline.analysisLevel`, `wardline.annotationGroups`, `wardline.dataSource`, `wardline.enclosingTier`, `wardline.excepted`, `wardline.exceptionability`, `wardline.rule`, `wardline.severity`, `wardline.taintState`). Contract schema at `src/wardline/manifest/schemas/wardline-sarif-extensions.schema.json`.

**Wardline does NOT use SARIF's own `partialFingerprints` or `baselineState`.** It emits its own `ast_fingerprint` scheme (SHA-256 over `ast.dump()`) into the separate `wardline.fingerprint.json`. Baseline compare uses `(ruleId, file, qualname)` tuples (`scan.py:1244-1252`).

**Output path**: user-chosen via `--output` (`scan.py:576`); no default filename. CI examples and site-src use `wardline.sarif` or `results.sarif`. The current Wardline repo's baseline file is `/home/john/wardline/wardline.sarif.baseline.json` (883 KB, 663 results).

**Not schema-validated programmatically on emission.** Correctness by construction + tests + property-bag schema.

**Wardline's "entity" model**. No persistent entity catalog. Transient per-scan structures:

- `Finding` (`scanner/context.py:22-55`) — one per violation
- `WardlineAnnotation` (`context.py:58-76`) — one discovered decorator
- `ScanContext` (`context.py:79-243`) — per-file state; `function_level_taint_map`, `annotations_map`, `project_annotations_map`, `module_file_map`, etc.
- `BoundaryEntry` (`manifest/models.py:202-231`) — manifest-declared boundary
- `FingerprintEntry` (`manifest/models.py:62-72`) — per-function baseline; persisted in `wardline.fingerprint.json`
- `ExceptionEntry` (`manifest/models.py:24-58`) — per-exception register

**Identity scheme**: `qualname` string (class/method dotted form — NOT Python `__qualname__`, which adds `<locals>` for nested functions) or `(file_path, qualname)` for project-wide views. File paths are absolute at runtime, POSIX-relative in SARIF output.

**Call graph**: built per-scan, thrown away (`scanner/taint/callgraph.py`, `callgraph_propagation.py`). No persistence.

**Tier vocabulary**: `INTEGRAL / ASSURED / GUARDED / EXTERNAL_RAW` (wardline.yaml configurable). **This does NOT match the design's glossary** which names tiers as `T1 (trusted assertion) / T2 (semantically validated) / T3 (guarded) / T4 (raw external) plus UNKNOWN / MIXED`. Either Wardline's names or the design's names are correct; they are not both. Design correction needed.

**What Wardline would gain or lose by pulling from Clarion**:

- **Gain**: AST/decorator-discovery deduplication; consistency of tier reporting between Clarion and Wardline; potentially faster incremental runs.
- **Lose**: ability to run without Clarion (currently zero HTTP dependencies, `dependencies = []` in `pyproject.toml:22`); ability to reason about work-in-progress code pre-commit; deterministic `inputHash` identity (today `_compute_input_hash` reads file bytes directly at `scan.py:103-151`).
- **Magnitude of refactor**: significant. `ScanEngine`, `ScanContext`, `ProjectIndex`, and every rule under `scanner/rules/` bind directly to in-memory AST structures — no `EntitySource` protocol exists. Honest label: "non-trivial architectural change."

### 2.5 Cross-tool data model overlaps (Question 5)

**Filigree owns (today)**: `file_records{id,path,language,file_type,first_seen,updated_at,metadata}`, `scan_findings{…}`, `file_associations{…}`, `file_events{…}`, `issues{…}`, `observations{…}`. **No content hashes, no qualnames, no sub-file entity granularity.** `metadata` is an opaque JSON blob; it has no SQL index.

**Wardline owns (today)**: per-function `FingerprintEntry{qualified_name, module, decorators, annotation_hash, tier_context}` persisted in `wardline.fingerprint.json`; per-exception `ExceptionEntry` in `wardline.exceptions.json`; manifest-declared `BoundaryEntry{function, transition, from_tier, to_tier, restored_tier}` from overlays; compliance ledger in `wardline.compliance.json`; conformance gates in `wardline.conformance.json`; SARIF baseline in `wardline.sarif.baseline.json`; retrospective-required state marker. **No SQLite anywhere** (`grep -r "CREATE TABLE" /home/john/wardline/src` returns 0).

**Clarion design owns**: entities, edges, findings, guidance sheets, file records, subsystems, summaries.

**Overlaps**:

| Data point | Filigree | Wardline | Clarion design |
|---|---|---|---|
| File path | `file_records.path` | `artifactLocation.uri` (transient) | File entity `core:file:{hash}@{path}` |
| File language | `file_records.language` | Computed per-scan | Plugin-emitted file-level tag |
| Function qualname | — | `FingerprintEntry.qualified_name` | Entity ID `python:function:{canonical_name}` — **different schemes** (`class.method` vs canonical-qualified-name) |
| Decorator set | — | `FingerprintEntry.decorators` | `decorated_by` edges |
| Tier declaration | — | `FingerprintEntry.tier_context` | Entity `wardline.tier` property |
| Content hash | — | — | Blake3 `content_hash` on entities |
| Annotation hash | — | `FingerprintEntry.annotation_hash` (SHA-256 over `ast.dump`) | Not named as such |
| Finding severity | `{critical,high,medium,low,info}` | `{ERROR,WARNING,SUPPRESS}` | `{INFO,WARN,ERROR,CRITICAL}` for defects, `NONE` for facts |
| Finding rule_id | free-text string | `PY-WL-*`, `SCN-*`, `SUP-*`, `TOOL-ERROR` enum | Namespaced `CLA-*`, `WL-*`, `COV-*` |
| Finding status | `{open,acknowledged,fixed,false_positive,unseen_in_latest}` | Carried via `result.suppressions[]` + `wardline.excepted` | `{open,acknowledged,suppressed,promoted_to_issue}` |

**Three distinct severity vocabularies across the suite** with no translation table in code. **Three distinct rule-ID namespace conventions.** **Two distinct content-hash schemes** (Wardline SHA-256 over `ast.dump`, Clarion Blake3 over content). All of these silently diverge.

**The suite currently deduplicates nowhere**. Filigree's `file_records` is its authoritative file list; Wardline's `FingerprintEntry` is its authoritative function list; they do not cross-reference.

**Where the suite accepts drift**: the `wardline.exceptions.json` register uses a free-text `location` string like `src/wardline/scanner/engine.py::ScanEngine._scan_file` (double-colon separated). This is a third identity scheme, different from Wardline's own `qualname` and from Clarion's planned ID.

### 2.6 Undocumented information flows (Question 6)

**Wardline → Filigree**: **none today.** Verified by:

- `grep -i filigree /home/john/wardline` (source + tests + scripts): zero matches in scanner path.
- `grep` for `requests|urllib|httpx|aiohttp|HTTPConnection` across `/home/john/wardline/src`: zero scanner-path matches. Only HTTP-capable code is `bar/adapters.py` using `litellm` for BAR LLM review — separate subsystem.
- `pyproject.toml:22` declares `dependencies = []`.
- CI (`.github/workflows/ci.yml:68-75`) uploads SARIF to **GitHub Security** via `github/codeql-action/upload-sarif@v3`, not to Filigree.

**The Clarion design's "Wardline findings flow to Filigree via Wardline's own integration; Clarion doesn't mediate" is false today.** If this flow is required by v0.1 acceptance, someone builds it.

**Filigree → Wardline**: none. Zero Filigree references in Wardline source beyond boilerplate `CLAUDE.md`/`AGENTS.md`. Filigree does not read `wardline.yaml`, `wardline.fingerprint.json`, or SARIF output.

**Git metadata use**:

- Filigree: one call in `install_support/doctor.py:620-650` running `git status --porcelain` for a dirty-tree warning. **No persistence of git data.**
- Wardline: `bar/evidence_exec.py` (`git rev-parse:66`, `git show:76`, `git log:224-230`, `git archive:313`) and `bar/runner.py:487` (`git diff-tree`). Used transiently for BAR evidence bundles. **Not persisted into scanner graph.**
- Clarion's planned git ingest (last_modified, churn_count, authors) duplicates nothing persistent today. Wardline's BAR subprocess layer is a potential reuse substrate but requires coordination.

**Observations are MCP-only in Filigree**. No HTTP endpoint exists for observation creation (`grep` confirms no route under `dashboard_routes/` for observations beyond list/get). The design repeatedly says "Clarion emits observations to Filigree" but is silent on the transport — Clarion must ship an MCP client that speaks `filigree.mcp`, or Filigree must add an `/api/v1/observations` HTTP route.

**Filigree's `.filigree/scanners/*.toml` extensibility surface** (`src/filigree/scanners.py`): Filigree-side `trigger_scan` launches registered scanners by name. If human operators should be able to trigger-scan Clarion from the Filigree dashboard, Clarion ships a `clarion.toml` scanner registration. The design does not name this.

**Commit-ref handling**: Wardline SARIF includes `wardline.commitRef` with `-dirty` suffix when tree is modified (real example: `f71ac90aac6df328bf94325cecab59bd92f72f51-dirty`). Clarion's "commit cadence" vocabulary does not specify whether it handles dirty trees, requires clean commits, or accepts either.

**Filigree's `register_file` side channel in `get_issue`**: `include_files=True` default at `mcp_tools/issues.py:368-370` means every `get_issue` call returns file associations. Under `registry_backend: clarion`, those file_ids are opaque Clarion entity IDs seen by agents reading the default response — a cross-tool flow the design does not acknowledge.

**`AGENTS.md` / ADR cross-references**: Wardline ADRs reference specific Filigree issue IDs by string (e.g., `wardline-63255c8d5a` in `ADR-006-sarif-suppress-as-native-suppression.md:280-281`). This is a documented-but-not-programmatic cross-tool coordination path. The design's glossary covers `filigree_issue_id` on Findings but not this authoring-time reference convention.

### 2.7 Integration test surface (Question 7)

**No cross-tool test infrastructure exists.** Verified by absence:

- `grep` for `wardline|sarif` in `/home/john/filigree/tests/`: zero files.
- `grep` for `filigree` in `/home/john/wardline/tests/`: zero files.
- No VCR cassettes, no recorded HTTP interactions, no shared JSON-schema files, no contract tests for schema versioning.
- `.agents/skills/filigree-workflow/` exists in both repos but is identical boilerplate about using Filigree as a task tracker.
- No mock Filigree server in Wardline tests; no mock Wardline SARIF output in Filigree tests.

**Filigree's canonical test scanner is Codex**, not Wardline. `scan_source="codex"` appears 28+ times across `tests/core/test_scans.py`.

**What would need to exist** to test Clarion ↔ Filigree ↔ Wardline end-to-end:

1. **A mock Filigree HTTP server** (acceptance criterion 12.8.1 already lists "A wardline mock client successfully consumes the HTTP read API" as a Clarion requirement — its inverse, "a mock Filigree successfully accepts Clarion POSTs," is not named).
2. **A Wardline SARIF corpus** — at least one file per annotation group, one per rule, for Clarion's plugin detection tests.
3. **A Filigree scan-results reference payload** — Clarion's emitter must round-trip through the real validation. Record `scripts/scan_utils.py:post_to_api` as a dependency, or inline equivalent test harness.
4. **A schema-compatibility contract test** — Filigree's `GET /api/files/_schema` should be pinned by Clarion's integration test (protocol drift detection).
5. **A Wardline `REGISTRY_VERSION` pin** — Clarion plugin detects version skew and emits `CLA-INFRA-WARDLINE-REGISTRY-STALE` (new rule).

None of these exist. Clarion v0.1 creates all of them.

---

## 3. Corrections required to the Clarion design

Each item quotes the design text, cites reality, and recommends an edit. Ordered by severity.

### 3.1 Finding-field names (factual, CRITICAL)

**Design text** (§3, line ~442; §9 line ~1487; Glossary line ~1757):
> the wire format between tools is `{scan_source, findings: [{path, line?, rule_id, severity, message, properties?}]}`
> Clarion's richer fields … travel as fields inside a `properties` bag on each finding

**Reality**: Filigree's extension slot is `metadata`, not `properties`. The line field is `line_start` + `line_end`, not `line`. Top-level keys outside the known set are silently dropped (`db_files.py:501-525`). Evidence: `_validate_scan_findings` at `db_files.py:386-428`, `_upsert_finding` at `:455-556`.

**Recommended edit**:

- Replace every occurrence of `properties?` with `metadata?` at finding level.
- Replace `line?` with `line_start?, line_end?`.
- Add an explicit note in §3 and Glossary: "Extension fields (`kind`, `confidence`, `confidence_basis`, `supports`, `supported_by`, `related_entities`) travel inside the `metadata` dict. Top-level unknown keys are dropped by Filigree ingest; nesting inside `metadata` round-trips and is queryable only via `GET /api/files/{id}/findings`, not via SQL filters."

### 3.2 Severity enum (factual, CRITICAL)

**Design text** (§3 Finding struct, line ~409):
> `severity: Severity, // INFO | WARN | ERROR | CRITICAL for defects; NONE for facts`

**Reality**: Filigree's severity enum is `{critical, high, medium, low, info}` (`types/core.py:14`). `WARN` and `ERROR` are not accepted — posting either coerces to `"info"` and surfaces a warning in the response (`db_files.py:411-427`). `CRITICAL` is uppercase in the design but lowercase `"critical"` on the wire.

**Recommended edit**:

- Define a severity mapping table in §3 or §9: `{INFO→info, WARN→medium, ERROR→high, CRITICAL→critical, NONE→info (with kind=fact in metadata)}` (or similar). Explicitly state that emissions use Filigree's wire vocabulary, not Clarion's internal type names.
- Add that Clarion must inspect `response.warnings[]` to detect severity coercion.

### 3.3 Wardline groups 9/12/13 manifest-declaration (factual, CRITICAL)

**Design text** (§2 Plugin manifest, lines ~238-244):
> Groups 9 (operations), 12 (Determinism), 13 (Concurrency) are declared separately in wardline.yaml rather than via source-level decorators; detection for those comes from the Wardline manifest ingest path, not the plugin.

**Reality**: All 17 groups are decorator-based. Groups 9/12/13 have 8 decorators between them in `core/registry.py:126-181` and are enforced by `sup_001.py` reading decorator-derived `WardlineAnnotation` records. `wardline.schema.json` and `overlay.schema.json` have no fields for these groups. Corpus confirms: `corpus/specimens/SUP-001/EXTERNAL_RAW/negative/SUP-001-TN-atomic.py:1-5` uses `@atomic`; `SUP-001-TN-not-reentrant.py:1-5` uses `@not_reentrant`.

**Recommended edit**:

- §2 manifest `wardline_groups_ast` must include groups 9, 12, 13: `[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17]`.
- Remove `wardline_groups_manifest: [9, 12, 13]`. Replace with a note that **Groups 1 and 17 have supplementary manifest declarations via overlay `boundaries[]`** (evidence: `overlay.schema.json:16-168`), and detection for those comes from both decorator-level (plugin) and manifest-level (core-side `wardline.yaml` ingest) paths.
- Remove the implication that any group is "manifest-only." None is.

### 3.4 Wardline tier vocabulary (factual, HIGH)

**Design text** (Glossary line ~1762):
> Wardline classification: T1 (trusted assertion), T2 (semantically validated), T3 (guarded), T4 (raw external). Plus UNKNOWN and MIXED.

**Reality**: Wardline's tier names are `INTEGRAL / ASSURED / GUARDED / EXTERNAL_RAW`, manifest-configurable in `wardline.yaml:tiers`. Evidence: `core/tiers.py`, `wardline.yaml:8-232`, `docs/spec/` Part I Part 1 tier taxonomy.

**Recommended edit**: Use Wardline's actual tier names in the glossary and in briefing vocabulary. If the design wants short codes, propose them to Wardline as an extension (`T_INTEGRAL` etc.), not by inventing `T1..T4`.

### 3.5 Wardline findings flow to Filigree today (factual, HIGH)

**Design text** (§9 Wardline integration table):
> Wardline enforcer → Filigree | Wardline's own integration | Wardline findings flow to filigree; Clarion doesn't mediate

**Reality**: Wardline has zero HTTP client code in its scanner path. Zero dependencies in `pyproject.toml:22`. Zero Filigree references. The CI pipeline uploads SARIF to **GitHub Security**, not Filigree. **This integration does not exist today.**

**Recommended edit**: Either (a) state explicitly that a Wardline→Filigree bridge is a precondition not yet satisfied, and list its owner (Wardline? Clarion? a separate adapter?); or (b) drop the claim and state that Wardline findings do not reach Filigree in v0.1 except via a yet-to-be-built translator.

### 3.6 Registry-backend flag does not exist (status clarity, HIGH)

**Design text** (§9):
> Filigree ships a `registry_backend: filigree | clarion` config flag.

**Reality**: `grep` for `registry_backend` across `/home/john/filigree` returns zero matches. This is a prerequisite not yet satisfied. Four NOT-NULL FK constraints in `db_schema.py` (at `:131`, `:161`, `:198`, `:214`) all point at `file_records(id)`. Three auto-create paths insert rows regardless of backend. Surgery estimate: ~5–8 hot files plus SQLite FK rework.

**Recommended edit**: Reframe §9 to state that `registry_backend` is a Filigree-side change Clarion *depends on*, not a Filigree feature it *uses*. Add a concrete Filigree-side work item (design + implementation) as an explicit dependency in the implementation plan. Name the three auto-create paths that must be routed through the pluggable backend.

### 3.7 MCP-only observation channel (status clarity, MEDIUM)

**Design text** (§9, §8):
> Clarion emits observations to Filigree.

**Reality**: Filigree's observation API is MCP-only. No HTTP endpoint exists. `mcp_tools/observations.py` is the only creation path. Clarion must ship an MCP client (stdio transport to Filigree's MCP server) to emit observations.

**Recommended edit**: §8 or §9 must state that Clarion ships an MCP client for Filigree (bidirectional: Clarion uses Filigree's MCP tools for observation emission, Clarion's own MCP server is a separate instance for agent consumption). Name the MCP transport (stdio subprocess of `filigree mcp` vs TCP) and auth posture.

### 3.8 HTTP routes parallel to displaced MCP tools (completeness, MEDIUM)

**Design text** (§9):
> Removed (for projects using Clarion): `register_file`, `list_files`, `get_file`, `get_file_timeline` MCP tools.

**Reality**: `dashboard_routes/files.py` exposes 11 HTTP routes that parallel these MCP tools, including `GET /files`, `GET /files/{id}`, `GET /files/{id}/timeline`, `GET /files/hotspots`, `GET /files/stats`. The dashboard UI uses these. Displacement at the MCP layer without HTTP-layer action creates silent inconsistency.

**Recommended edit**: Either (a) state that the HTTP routes are also displaced/re-routed under `registry_backend: clarion`, or (b) state explicitly that MCP displacement is cosmetic and the dashboard continues to show Filigree-native file records. Pick one.

### 3.9 `get_issue` default-embed leak (completeness, MEDIUM)

**Design text** (§9): "retained with opaque entity IDs."

**Reality**: `mcp_tools/issues.py:368-370` has `include_files=True` by default; every `get_issue` call returns file associations via `get_issue_files`. Under `registry_backend: clarion`, agents reading the default issue response suddenly see opaque entity IDs in a field they did not request.

**Recommended edit**: Note this in §9: "Because `get_issue_files` is default-embedded by `get_issue`, the 'retained but opaque' framing applies to any agent reading an issue. Clarion must document that `file_id` on issue-detail responses is a Clarion entity ID string, not a Filigree-native ID."

### 3.10 `_schema` does not enumerate scan_source (factual, LOW)

**Design text** (§9): "treat Filigree's `GET /api/files/_schema` as the source of truth for enums (severity values, valid `scan_source` identifiers)."

**Reality**: `_schema` at `files.py:94-175` is source of truth for `valid_severities`, `valid_finding_statuses`, `valid_association_types`, and sort-field enums. **It does NOT list `scan_source` values** — `scan_source` is a free-form string server-side.

**Recommended edit**: Replace "valid `scan_source` identifiers" with "severity values, finding statuses, association types." Acknowledge that `scan_source` is free-form and that suite-wide coordination of scan_source names is a social convention, not a server-enforced enum.

---

## 4. Gaps — integration concerns the design does not yet name

These are not corrections to existing text; they are missing sections or decisions.

### 4.1 Wardline SARIF `wardline.*` property-bag translation layer

Wardline's SARIF output carries 35 run-level and 9 result-level `wardline.*` extension keys (`wardline-sarif-extensions.schema.json`). Examples: `wardline.controlLaw` (states: `normal|alternate|direct`), `wardline.taintState`, `wardline.excepted`, `wardline.enclosingTier`, `wardline.governanceEvents`. Any bridge that translates Wardline SARIF into Filigree's flat intake either preserves these into `metadata` (Clarion must standardise the nesting) or loses fidelity.

**Design action**: name who owns the SARIF→Filigree translator, where it lives, and whether the `wardline.*` keys are preserved in `metadata.wardline_properties.*` or flattened.

### 4.2 Severity and rule-ID mapping tables

Three severity vocabularies, three rule-ID namespace conventions, zero translation tables in code. Design §3 names "CLA-PY-001 | WL-001 | COV-001" but does not define how Wardline's `PY-WL-001-GOVERNED-DEFAULT` (hyphenated, longer) round-trips through Filigree's free-text `rule_id` column while remaining parseable back to the original.

**Design action**: publish a severity-mapping matrix and a rule-ID round-trip policy in §9.

### 4.3 Wardline persistent state files the design does not ingest

Beyond `wardline.yaml`, Wardline persists authoritative state in:

- `wardline.fingerprint.json` — per-function `{qualified_name, module, decorators, annotation_hash, tier_context}`. **Most aligned with Clarion's entity model.**
- `wardline.exceptions.json` — exception register with expiry.
- `wardline.compliance.json` — compliance ledger.
- `wardline.conformance.json` — conformance gates.
- `wardline.perimeter.baseline.json` — perimeter fingerprint.
- `wardline.manifest.baseline.json` — manifest fingerprint.
- `wardline.retrospective-required.json` — retrospective state marker.

§2 says "Clarion ingests `wardline.yaml` + overlays." **Omitting `wardline.fingerprint.json` means Clarion's Wardline-derived guidance misses the computed per-function state.** Omitting the exceptions register means Clarion does not know which entities have active excepted findings.

**Design action**: §2 and §7 should name which of these files Clarion ingests. At minimum, `wardline.fingerprint.json` should be in scope; overlays for boundaries[] (groups 1 and 17) are already in scope if "overlays" means the overlay files in general. The other five need an explicit in/out decision.

### 4.4 `REGISTRY_VERSION` compatibility mechanism

Wardline's `core/registry.py:10` carries a `REGISTRY_VERSION` string. Clarion's mirror (or its direct import) can detect version skew. The design's v0.2 "Wardline annotation descriptor" does not name this. Today, a new Wardline annotation requires a Clarion plugin release; a version-skew detector would at least emit `CLA-INFRA-WARDLINE-REGISTRY-STALE` when Wardline bumps and Clarion lags.

**Design action**: specify version-skew detection as a v0.1 mechanism, independent of whether the annotation descriptor is a v0.2 file. Clarion reads `wardline.core.registry.REGISTRY_VERSION` at plugin startup (direct Python import is feasible; `REGISTRY` is a `MappingProxyType` with no heavy deps) and emits a finding on mismatch.

### 4.5 Wardline's legacy decorator aliases

`LEGACY_DECORATOR_ALIASES = MappingProxyType({"tier_transition": "trust_boundary"})` at `core/registry.py:12-14`. Clarion's plugin keyed only on source-level name would double-count. Wardline already resolves via this table.

**Design action**: §2 Python plugin specifics table on decorator detection should add a row: "Legacy aliases (`tier_transition`) — resolved via Wardline's `LEGACY_DECORATOR_ALIASES`; canonical name recorded in `decorated_by.properties.canonical_name`."

### 4.6 Class decoration goes beyond what Wardline itself detects

Wardline's `scanner/discovery.py:_walk_functions` only visits `FunctionDef`/`AsyncFunctionDef`. Class-level `@validates_shape` is ignored by Wardline's own scanner. Clarion's design promises to detect class decorators "identically to function decorators." This is a policy decision — Clarion goes beyond Wardline's own coverage.

**Design action**: §2 should note that class-level decoration is Clarion-side augmentation, not Wardline-authoritative. Wardline-side rules do not enforce class-level annotations. Clarion's emitted findings for class-level decoration must carry a `confidence_basis: "clarion_augmentation"` tag distinguishing them from Wardline-authoritative claims.

### 4.7 Auto-create paths in Filigree

Three auto-create paths in Filigree insert `file_records` rows regardless of source:

1. `POST /api/v1/scan-results` → `_upsert_file_record` (`db_files.py:430-453`).
2. `create_observation(file_path=…)` → `register_file` call (`db_observations.py:135-147`).
3. `trigger_scan` → `tracker.register_file(...)` (`mcp_tools/scanners.py:422, 586`).

Under `registry_backend: clarion`, these paths either continue to write `file_records` (now a shadow table) or must route through a new `RegistryProtocol`. The design's "displace the registry" story elides all three.

**Design action**: §9 must name each auto-create path and specify the behaviour under displacement. Preferred: keep `file_records` as a lightweight shadow index (path → opaque_id mapping) and let Clarion be authoritative; do not claim to "remove" the Filigree table.

### 4.8 Observation transport (MCP vs HTTP)

Filigree's observation API is MCP-only. Clarion's "emits observations to Filigree" claim implies an MCP client on Clarion's side.

**Design action**: name the MCP transport (Clarion spawns `filigree mcp` as a subprocess? Clarion uses an existing MCP host?), the auth posture (does Filigree's MCP server need a token?), and the failure mode (what happens when Filigree's MCP is unreachable during `clarion analyze`?).

### 4.9 Scanner registration for operator-triggered scans

Filigree's `.filigree/scanners/*.toml` extensibility lets the dashboard `trigger_scan` a named scanner. If a user wants to trigger Clarion from the Filigree dashboard — a natural affordance given the shared MCP/observation model — Clarion must register a `clarion.toml`.

**Design action**: decide whether operator-triggered scans via Filigree's dashboard are in v0.1 scope. If yes, specify the TOML contents (executable, args, output-path, scan_source identifier). If no, note it as explicitly deferred so the absence isn't read as an oversight.

### 4.10 `scan_run_id` lifecycle

Filigree tracks `scan_runs` rows with state transitions. `POST /api/v1/scan-results` with `scan_run_id` set AND `complete_scan_run=true` attempts to close the run; unknown `scan_run_id` logs a warning and continues. Clarion's `run_id` (§3 Finding.source.run_id) is a separate concept.

**Design action**: decide whether Clarion's `run_id` maps onto Filigree's `scan_run_id` (requires `create_scan_run` call before emission) or whether Clarion posts without a `scan_run_id` and declines the run-lifecycle feature. Either is fine; silence produces drift.

### 4.11 Dedup key collision under same-rule-same-line findings

Filigree's finding dedup is `(file_id, scan_source, rule_id, coalesce(line_start, -1))`. Clarion's per-entity findings may re-post the same `(path, rule_id)` across runs with different `line_start` values (because entities move within a file). The current dedup collapses cross-run history at the same line; an entity renamed and moved inside a file loses its triage state.

**Design action**: specify Clarion's dedup key policy — either (a) accept Filigree's dedup and live with the coarseness, (b) synthesise `rule_id` to include the entity ID (e.g., `CLA-PY-STRUCTURE-001:python:class:auth.tokens.TokenManager`), (c) push dedup server-side into Filigree as a new feature.

### 4.12 Integration test fixtures and mocks

Acceptance criterion 12.8.1 names "a wardline mock client successfully consumes the HTTP read API." Its inverse — "a mock Filigree accepts Clarion's POSTs with richer `metadata` nested fields" — is not named. Nor is a Wardline SARIF corpus for Clarion plugin detection tests.

**Design action**: §12 Testing should add fixture requirements: (a) mock Filigree HTTP server (axum/wiremock in Rust); (b) Wardline SARIF corpus (a committed slice of real Wardline output); (c) `REGISTRY_VERSION` pinning test; (d) schema-compatibility test pinning `GET /api/files/_schema`.

### 4.13 Commit-ref dirty handling

Wardline SARIF includes `wardline.commitRef` with `-dirty` suffix when the tree is modified. Clarion's commit-cadence vocabulary is silent.

**Design action**: specify Clarion's policy: does `clarion analyze` refuse to run on dirty trees, record the dirty marker on the run, or strip it?

### 4.14 BAR subsystem awareness

Wardline's BAR subsystem (`src/wardline/bar/`) already has an LLM-reviewer pipeline using `litellm`/`anthropic`, producing `bar_evidence.json` artifacts. It is adjacent to but independent of Clarion's LLM orchestration.

**Design action**: not necessarily v0.1 scope, but the design should acknowledge BAR exists to avoid later surprise. At minimum, non-goals (§10) should list "BAR-style reviewer sessions" so the separation is explicit.

### 4.15 Identity scheme alignment for Wardline qualnames

Wardline's `qualname` is the nested-class-method dotted form (e.g., `ScanEngine._scan_file`). Clarion's design specifies `canonical_qualified_name` with Python package path and `src/` stripping (e.g., `wardline.scanner.engine.ScanEngine._scan_file`). These are **different strings**.

**Design action**: §3 must specify the Wardline-identity mapping. Either Clarion ingests `fingerprint.json` qualnames and resolves them to Clarion entity IDs via `module_file_map`, or Clarion exposes a `wardline_qualname` property on entities and accepts non-unique joins.

### 4.16 Filigree CHANGELOG-signalled breaking changes

Filigree's CHANGELOG (`CHANGELOG.md:91`) explicitly uses "Breaking (API)" markers and has had recent changes to the scan-results endpoint shape (`create_issues` → `create_observations` in v2.0.0 Unreleased). Filigree does not treat `/api/v1/…` as a strong stability contract; it breaks and tags.

**Design action**: §9 should note Filigree's breaking-change posture and specify Clarion's policy on tracking Filigree's CHANGELOG (CI-time schema-compatibility test is the minimum).

---

## 5. Confidence and caveats

### 5.1 What was verified

- **Filigree scan-results intake shape, location, and behaviour** — read in full: `dashboard_routes/files.py:94-175, 294-330`; `db_files.py:386-725`; `db_schema.py:116-214`; `types/core.py:14-16`; `types/files.py:138-148`; `models.py:125-175`; `scripts/scan_utils.py:236-382`; tests in `tests/core/test_files.py:496-540` and `tests/api/test_files_dashboard.py:136-437`.
- **Filigree MCP tool inventory and handlers** — read in full: `mcp_tools/files.py` (entire file); `mcp_server.py:155-163`.
- **Four FK constraints on `file_records(id)`** — `db_schema.py:131, 161, 198, 214`.
- **Three auto-create paths** — traced from each ingress point to `_upsert_file_record`.
- **Absence of `registry_backend`, `FILIGREE_FILE_REGISTRY_DISPLACED`, `clarion` references in Filigree** — `grep` across full tree.
- **Absence of Wardline references in Filigree**, and **absence of Filigree references in Wardline scanner path** — `grep` across both trees.
- **Wardline decorator inventory (42 + 1)** — read `core/registry.py:55-237`, `decorators/__init__.py:46-89`, and each decorator module. Cross-checked against `docs/spec/wardline-01-07-annotation-vocabulary.md:13-32` and corpus specimens.
- **Wardline SARIF shape and property-bag extensions** — read `scanner/sarif.py` in full; verified against `wardline.sarif.baseline.json` (883 KB, 663 results); cross-checked extension schema at `wardline-sarif-extensions.schema.json`.
- **Wardline's scanner has zero HTTP-client code** — `grep` for `requests|urllib|httpx|aiohttp|HTTPConnection|urlopen` across `src/wardline` returned zero matches in scanner path; `pyproject.toml:22` dependencies list.
- **No cross-tool test infrastructure** — `grep` across both `tests/` directories.

### 5.2 What was inferred (labelled)

- **"Surgery estimate ~5–8 hot files" for `registry_backend`** — based on call-site counts and FK topology. I did not sketch an actual `RegistryProtocol` refactor; the estimate could be lower with a clean protocol or higher if `ObservationsMixin` coupling proves sticky.
- **"Wardline accepting Clarion as entity source is a significant refactor"** — inferred from reading `ScanEngine`, `ScanContext`, `ProjectIndex` structures; not from attempting the refactor.
- **"`output_schema` is bare in code, factory in spec"** — inferred from reading `schema.py:53` (no `*args` capture) and spec text at `wardline-02-A-python-binding.md:199`. May be intentional (the spec describes a forward-compatible factory form); Clarion should accept both.
- **"SARIF property-bag translation is non-trivial"** — 35 + 9 = 44 keys with interlocking semantics. Inferred complexity from cross-references in `sarif.py`, not from attempting the translation.
- **"Wardline findings flow to Filigree via Wardline's own integration — false as of today"** — verified by absence of code. If an out-of-tree bridge exists (user's own scripts, a downstream project), I could not detect it.

### 5.3 What I could not reach

- **Any Wardline adapter that might live outside `/home/john/wardline`** — if the user maintains a `wardline-filigree-bridge` project elsewhere, it is invisible to this recon.
- **The Filigree dashboard's JavaScript** — only backend route handlers examined. If the dashboard UI has its own file-registry assumptions (e.g., integer IDs in URLs), I did not surface them.
- **Live Anthropic SDK model-ID set** — the design's Rev 1 `claude-opus-4-7` naming was flagged by the earlier review. I did not verify the current Anthropic SDK's accepted IDs; the design's Rev 2 "pinned against Anthropic SDK with CI-guard note" is an acceptance criterion for implementation, not something I could check here.
- **Runtime behaviour of Filigree's `_schema` under unusual requests** — I read the handler but did not exercise it.
- **Runtime behaviour of Wardline's `--changed-only` flag** — read the flag definition at `scan.py:599, 760-767` but did not trace the git-diff path in full.

### 5.4 Rough-edge items

- Filigree's severity coercion (permissive, warn-on-unknown) and Wardline's severity enum (`ERROR/WARNING/SUPPRESS`) **disagree on posture** — Filigree wants to keep data flowing; Wardline wants strict validation. Clarion's posture should be stated explicitly.
- Filigree treats `scan_source` as free-form, which lets Clarion and Wardline coexist on the endpoint without coordination, but also lets two scanners silently claim the same name. This is "ambient tribal convention," not enforced protocol.
- The three persistent identity schemes (Wardline's `qualname`, Wardline's exception-register `location` string, Clarion's planned `canonical_qualified_name`) are **three different answers to "what is this code element?"** — unifying them is real work, not incidental.

---

*End of integration reconnaissance.*
