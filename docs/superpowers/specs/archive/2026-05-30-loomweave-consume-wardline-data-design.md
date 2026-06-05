# Loomweave consumes Wardline data — design

**Date:** 2026-05-30
**Status:** approved (brainstorm directive: "step up, Wardline drops today");
deliverable is **spec + tracked issues** (design now, build next).
**Scope decision:** both Wardline-consume flows. Flow B (read-time finding
reconciliation) is designed in full here; Flow A (extraction-time NG-25 probe
annotation) is re-scoped in the tracker and cross-referenced, not redesigned.
**Builds on:** ADR-018 (qualname divergence / asterisk 2), ADR-015 (Wardline→
Filigree emission), the federation contract `docs/federation/contracts.md`
§"Wardline qualname normalization (entity reconciliation)", and Wardline SP4
(Outputs + Weft Integration, 2026-05-30).

---

## 1. Problem

Wardline SP4 ships a native Filigree emitter today: `POST /api/weft/scan-results`
with `scan_source="wardline"`, each finding carrying `metadata.wardline.qualname`
(the pre-composed dotted `module.qualified_name`, i.e. Loomweave's L7
`canonical_qualified_name`). SP4 §10 makes entity-association emission a Wardline
**non-goal** — Wardline does **not** emit a Loomweave `entity_id`.

Loomweave's only existing consume hook (`issues_for` / `orientation_pack`) is
**entity_id-keyed**: `GET /api/entity-associations?entity_id=<id>` → then
`GET /api/weft/issues/{id}`. Wardline findings are qualname-keyed with no
`entity_id`, so nothing currently surfaces them on a Loomweave entity. The
`entities/resolve?scheme=wardline_qualname` oracle named in ADR-018 is deferred
and unimplemented. The result is a consume-side blind spot the moment Wardline
data starts landing in Filigree.

There are two distinct flows under "consume Wardline data":

- **Flow A — extraction-time annotation.** The Python plugin reads Wardline's
  NG-25 descriptor at `loomweave analyze` time and stamps each entity's reserved
  `wardline` column (`crates/loomweave-storage/migrations/0001_initial_schema.sql`,
  `EntityRecord.wardline_json`). Today the probe
  (`plugins/python/.../wardline_probe.py`) proves only the import/version
  handshake; the column is all-`None`. Tracked as `clarion-1f6241b329` +
  `clarion-22acf15fd7`.
- **Flow B — read-time finding reconciliation.** Ingest the Wardline *findings*
  SP4 emits into Filigree, reconcile `qualname → entity`, and surface them
  through `issues_for` / `orientation_pack`. Unbuilt, untracked. **This is what
  drops today**, and the primary subject of this spec.

## 2. The simplifying insight

`metadata.wardline.qualname` *is* Loomweave's L7 `canonical_qualified_name`, which
is literally **segment 3 of the entity_id** (`{plugin}:{kind}:{qualname}`, e.g.
`python:function:pkg.mod.func`). So reconciling a Wardline finding to a Loomweave
entity is a **local lookup against Loomweave's own catalog** — compose
`python:function:<wardline.qualname>` and look it up — not a network round-trip
to a resolve oracle. Loomweave owns its catalog; it never needs to ask a sibling
"does this qualname resolve?". This removes the largest piece of perceived work
and keeps the matching logic (and the documented divergence traps) on the side
that owns the truth.

## 3. Flow B design — read-time lazy reconciliation

Enrich-only, parallel to the existing entity-association lookup, invoked when
`issues_for` / `orientation_pack` runs for an entity **E** (qualname `Q`, file
`F`):

1. **Fetch** Wardline findings *scoped by file `F`* from Filigree (see §4).
2. **Resolve each finding to an entity, locally.** For each fetched finding,
   normalize its `metadata.wardline.qualname` and look it up in Loomweave's catalog
   (a pure string compare — no network). Tag the finding with how it resolved:
   - `exact` — qualname matches an indexed entity. Bind the finding to that
     entity.
   - `heuristic` — qualname does not match an indexed entity exactly but a
     best-effort normalization resolves to one. Bind with lowered confidence.
   - `none` — resolves to no indexed entity (the file matched, the symbol did
     not). File-level only; not entity-bound.
3. **Attach.** For a query on entity E, surface the findings that resolved
   (`exact` or `heuristic`) **to E** under a new `wardline_findings` section:
   `rule_id`, `message`, `severity`, `line_start/end`, `fingerprint`, `status`,
   the `metadata.wardline.*` block (kind, confidence, suppression), and the
   finding's `resolution_confidence`. Findings in `F` that resolved to a
   *different* entity belong to that entity (not E); `none` findings are
   available as file-level context, never bound to E.

This mirrors the existing `issues_for` enrich-only behavior exactly: Filigree is
an enrichment source, never load-bearing.

### Kind handling & divergence traps

Compose `python:function:<qualname>`. Wardline emits **functions/methods only**
(SP4 §6); methods carry dotted qualnames and remain `kind=function`. Class- and
module-targeted findings are **out of scope in v1** (Wardline does not emit them)
— a documented limitation, not a silent drop. Matching honors the normative
divergence traps from `fixtures/wardline-qualname-normalization.json`: `None ↔ ""`
for a rejected top-level `__init__.py`, non-`src` roots not stripped, `src` only
stripped at position 0, `<locals>` closure markers and nested-class chains
preserved verbatim.

## 4. Data source — composed from existing Filigree routes (no new contract)

**Verified against Filigree source (2026-05-30): no new Filigree route is
needed.** Flow B composes two existing Filigree *weft* read routes. (Note: the
`POST /api/v1/files:resolve` route in `contracts.md` is a route **Loomweave
exposes** — it returns *Loomweave* entity_ids for paths — not a Filigree route
Loomweave consumes; it is the wrong direction for this and is not used here.)

1. **`GET /api/weft/files?scan_source=wardline&path_prefix=<E.source_file_path>`**
   — `api_weft_list_files` (`filigree/dashboard_routes/files.py`); filters
   include `scan_source` and `path_prefix`. Returns `FileRecordWeft` items
   carrying `file_id` + `path`. Loomweave takes the item whose `path` **exactly**
   equals `E.source_file_path` (`path_prefix` is a prefix, so an exact-match
   filter is required) → Filigree `file_id`. Pinned by
   `tests/fixtures/contracts/weft/files.json`.
2. **`GET /api/weft/findings?scan_source=wardline&file_id=<file_id>`** —
   `api_weft_list_findings`; filters include `scan_source`, `status`, `file_id`.
   Each row is a `ScanFindingWeft` carrying `rule_id`, `message`, `severity`,
   `status`, `line_start/line_end`, `fingerprint`, `file_id`, and **`metadata`**
   (where `metadata.wardline.qualname` lives). Pinned by
   `tests/fixtures/contracts/weft/findings.json`.

So the fetch is: `weft/files?path_prefix=<path>` → exact-path match → `file_id`,
then `weft/findings?scan_source=wardline&file_id=<id>`, then the local qualname
match in §3. `ScanFindingWeft` rows reference the file by `file_id` (not `path`),
which is why the file-list hop is required; file-scoping also keeps each query
bounded rather than sweeping all project findings.

This is **Loomweave-side build only** — the MCP Filigree client (`filigree.rs`)
gains a weft-files-list call and a weft-findings-list call; no Filigree-side work,
and **no federation contract request** (both routes already exist — checked
against Filigree source, not assumed; cf. the withdrawn prune ask in `7a93883`).
Enrich-only still holds: if either route is unreachable, the `wardline_findings`
section degrades to empty + `degraded`.

## 5. Error handling (enrich-only, no fabrication)

- **Filigree absent / unreachable** → degrade to an empty `wardline_findings`
  section flagged `degraded`; the tool never fails. `result_kind` distinguishes
  `matched` / `no_matches` / `unavailable`, so a reachable-but-empty Filigree is
  never confused with an unreachable one (existing `issues_for` discipline).
- **Malformed finding** (missing/empty `metadata.wardline.qualname`, or a
  qualname that fails normalization) → skip it, increment a count in an
  `omitted` block; never crash the tool.
- **No fabrication.** An empty section is reported as empty with its cause, in
  keeping with the `scope_excludes` / `degraded` discipline already used across
  the MCP surface.

## 6. Testing (hermetic)

Unit tests with an **injected Filigree transport** returning canned Wardline
findings — no live server in the unit suite:

- qualname match including every divergence trap (`None ↔ ""`, non-`src` roots,
  `<locals>`, nested-class chains);
- the three `resolution_confidence` tiers (`exact` / `heuristic` / `none`);
- enrich-only degradation: transport error → `degraded`, tool returns, no panic;
- no-fabrication: reachable-but-empty → `no_matches`, not a synthesized entry;
- malformed-metadata finding → skipped + counted in `omitted`.

An optional live e2e (against an already-running Filigree) is out of the unit
suite.

## 7. Flow A coordination (re-scope, not redesigned here)

Per Wardline SP4 §2, Wardline now **emits** the NG-25 descriptor (SP2d shipped),
but Loomweave's **reader** is unbuilt and the plugin still imports
`wardline.core.registry` directly. The external blocker on `clarion-1f6241b329`
(*"Wardline SP2 finalizing the NG-25 descriptor shape"*) has therefore **lifted**
— it is now actionable Loomweave work (build the descriptor reader; populate the
`wardline` column at `analyze` time), no longer gated on Wardline.

Action (tracker only, this session): update `clarion-1f6241b329` and
`clarion-22acf15fd7` to record the descriptor ship, remove the "blocked on
Wardline" framing, and cross-reference the two flows as one Wardline-enrichment
story — extraction-time annotation (`wardline` column) and read-time findings
(this spec) feed the same goal from opposite ends. Flow A keeps its own design
when it is built.

## 8. Non-goals

- No `entities/resolve?scheme=wardline_qualname` oracle (deferred; Loomweave's
  consume path matches locally, so the oracle is not on Flow B's critical path).
- No entity-association write-back to Filigree (ADR-029) — Approach 2 from the
  brainstorm was rejected; Flow B is read-time-only, no Loomweave-initiated mutation
  of Filigree state.
- No class/module-targeted Wardline findings (Wardline emits functions/methods).
- No Flow A implementation in this spec (re-scope only).
- No new runtime dependency — reuses the existing Filigree HTTP client in
  `crates/loomweave-mcp/src/filigree.rs`.

## 9. Risks

- **R1 — sibling-route shape drift.** Both consumed routes (`files:resolve`,
  `GET /api/weft/findings`) exist today (verified), but their wire shape could
  drift. Mitigated by pinning the consumed shapes in
  `docs/federation/contracts.md` against Filigree's normative fixtures
  (`tests/fixtures/contracts/weft/findings.json`) and testing the client against
  that fixture, not a guess. No new route is requested, so there is no
  ahead-of-build dependency to land.
- **R2 — qualname mismatch surfaced as silent miss.** A producer divergence
  (Wardline composing a qualname Loomweave can't match) degrades to
  `resolution_confidence: none`. Mitigated by the shared normative fixture and by
  surfacing `none`/`heuristic` counts rather than dropping them.
- **R3 — enrich path inflating query latency.** A per-entity Filigree round-trip
  on every `issues_for` / `orientation_pack`. Mitigated by file-scoped fetch
  (one request per file, reusable across entities in that file) and the
  enrich-only timeout/degrade already in the Filigree client.

## 10. Deliverables (this session)

1. This spec, committed.
2. New `release:1.1` issue — **Flow B: read-time Wardline finding
   reconciliation** — citing this spec, ADR-018, and contracts.md §reconciliation.
   **Not** gated on any Filigree-side work (both consumed routes already exist);
   it is straightforward Loomweave-side build, sequenced whenever picked up.
3. Pin the two consumed weft read routes (`GET /api/weft/files?scan_source=…&
   path_prefix=…` for path→file_id, and `GET /api/weft/findings?scan_source=…&
   file_id=…`) in `docs/federation/contracts.md` as part of the Flow B build.
   **No** federation contract request is filed — the routes already exist
   (verified against Filigree source), so a request would be moot (cf. the
   withdrawn prune ask in `7a93883`).
4. Re-scoped `clarion-1f6241b329` + `clarion-22acf15fd7` (Flow A unblock +
   cross-reference).

Any ADR minted from this work (e.g. an ADR-018 amendment pinning the read-time
consume mechanism) is kept under `docs/loomweave/adr/` per the editorial
conventions, authored when the decision locks at build time.

## 11. References

- ADR-018 — L7 qualname divergence with Wardline FingerprintEntry (asterisk 2).
- ADR-015 (Revision 2) — Wardline→Filigree native emission.
- `docs/federation/contracts.md` §"Wardline qualname normalization (entity
  reconciliation)" and §"Consumed Filigree route: issue detail (enrichment)".
- `docs/federation/fixtures/wardline-qualname-normalization.json` — normative
  qualname parity vectors.
- Wardline SP4 — Outputs + Weft Integration (2026-05-30) §2, §6, §10.
- `crates/loomweave-mcp/src/filigree.rs` — existing enrich-only Filigree client.
