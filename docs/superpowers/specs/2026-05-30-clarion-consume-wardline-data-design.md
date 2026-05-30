# Clarion consumes Wardline data — design

**Date:** 2026-05-30
**Status:** approved (brainstorm directive: "step up, Wardline drops today");
deliverable is **spec + tracked issues** (design now, build next).
**Scope decision:** both Wardline-consume flows. Flow B (read-time finding
reconciliation) is designed in full here; Flow A (extraction-time NG-25 probe
annotation) is re-scoped in the tracker and cross-referenced, not redesigned.
**Builds on:** ADR-018 (qualname divergence / asterisk 2), ADR-015 (Wardline→
Filigree emission), the federation contract `docs/federation/contracts.md`
§"Wardline qualname normalization (entity reconciliation)", and Wardline SP4
(Outputs + Loom Integration, 2026-05-30).

---

## 1. Problem

Wardline SP4 ships a native Filigree emitter today: `POST /api/loom/scan-results`
with `scan_source="wardline"`, each finding carrying `metadata.wardline.qualname`
(the pre-composed dotted `module.qualified_name`, i.e. Clarion's L7
`canonical_qualified_name`). SP4 §10 makes entity-association emission a Wardline
**non-goal** — Wardline does **not** emit a Clarion `entity_id`.

Clarion's only existing consume hook (`issues_for` / `orientation_pack`) is
**entity_id-keyed**: `GET /api/entity-associations?entity_id=<id>` → then
`GET /api/loom/issues/{id}`. Wardline findings are qualname-keyed with no
`entity_id`, so nothing currently surfaces them on a Clarion entity. The
`entities/resolve?scheme=wardline_qualname` oracle named in ADR-018 is deferred
and unimplemented. The result is a consume-side blind spot the moment Wardline
data starts landing in Filigree.

There are two distinct flows under "consume Wardline data":

- **Flow A — extraction-time annotation.** The Python plugin reads Wardline's
  NG-25 descriptor at `clarion analyze` time and stamps each entity's reserved
  `wardline` column (`crates/clarion-storage/migrations/0001_initial_schema.sql`,
  `EntityRecord.wardline_json`). Today the probe
  (`plugins/python/.../wardline_probe.py`) proves only the import/version
  handshake; the column is all-`None`. Tracked as `clarion-1f6241b329` +
  `clarion-22acf15fd7`.
- **Flow B — read-time finding reconciliation.** Ingest the Wardline *findings*
  SP4 emits into Filigree, reconcile `qualname → entity`, and surface them
  through `issues_for` / `orientation_pack`. Unbuilt, untracked. **This is what
  drops today**, and the primary subject of this spec.

## 2. The simplifying insight

`metadata.wardline.qualname` *is* Clarion's L7 `canonical_qualified_name`, which
is literally **segment 3 of the entity_id** (`{plugin}:{kind}:{qualname}`, e.g.
`python:function:pkg.mod.func`). So reconciling a Wardline finding to a Clarion
entity is a **local lookup against Clarion's own catalog** — compose
`python:function:<wardline.qualname>` and look it up — not a network round-trip
to a resolve oracle. Clarion owns its catalog; it never needs to ask a sibling
"does this qualname resolve?". This removes the largest piece of perceived work
and keeps the matching logic (and the documented divergence traps) on the side
that owns the truth.

## 3. Flow B design — read-time lazy reconciliation

Enrich-only, parallel to the existing entity-association lookup, invoked when
`issues_for` / `orientation_pack` runs for an entity **E** (qualname `Q`, file
`F`):

1. **Fetch** Wardline findings *scoped by file `F`* from Filigree (see §4).
2. **Resolve each finding to an entity, locally.** For each fetched finding,
   normalize its `metadata.wardline.qualname` and look it up in Clarion's catalog
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

## 4. External dependency — Filigree findings-read route (contract ask)

Flow B needs a Filigree read surface Clarion does not yet call: **fetch Wardline
findings for a file path.** Proposed contract:

```text
GET {filigree_base}/api/loom/findings?scan_source=wardline&path=<rel-path>
  → { findings: [ { rule_id, message, severity, line_start, line_end,
                    fingerprint, status, metadata } ... ],
      result_kind: matched | no_matches | unavailable }
```

Fetching **by `path`** (a first-class top-level column in Filigree's intake)
rather than by nested `metadata.wardline.qualname` keeps the sibling from having
to index nested JSON, and leaves the precise qualname match on Clarion's side.

This route is **not assumed to exist**. Filigree exposes `list_findings` /
`get_finding` as MCP tools; the exact HTTP read shape must be confirmed. The
deliverable below files this as a federation contract request (same pattern as
the prune-unseen / scan-run-create request in commit `7a93883`), and Flow B
implementation is gated on it. Until the route lands, Flow B has no live data
source — which is acceptable: enrich-only means "absent → empty section", not
"broken".

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
but Clarion's **reader** is unbuilt and the plugin still imports
`wardline.core.registry` directly. The external blocker on `clarion-1f6241b329`
(*"Wardline SP2 finalizing the NG-25 descriptor shape"*) has therefore **lifted**
— it is now actionable Clarion work (build the descriptor reader; populate the
`wardline` column at `analyze` time), no longer gated on Wardline.

Action (tracker only, this session): update `clarion-1f6241b329` and
`clarion-22acf15fd7` to record the descriptor ship, remove the "blocked on
Wardline" framing, and cross-reference the two flows as one Wardline-enrichment
story — extraction-time annotation (`wardline` column) and read-time findings
(this spec) feed the same goal from opposite ends. Flow A keeps its own design
when it is built.

## 8. Non-goals

- No `entities/resolve?scheme=wardline_qualname` oracle (deferred; Clarion's
  consume path matches locally, so the oracle is not on Flow B's critical path).
- No entity-association write-back to Filigree (ADR-029) — Approach 2 from the
  brainstorm was rejected; Flow B is read-time-only, no Clarion-initiated mutation
  of Filigree state.
- No class/module-targeted Wardline findings (Wardline emits functions/methods).
- No Flow A implementation in this spec (re-scope only).
- No new runtime dependency — reuses the existing Filigree HTTP client in
  `crates/clarion-mcp/src/filigree.rs`.

## 9. Risks

- **R1 — sibling-route shape drift.** The Filigree findings-read route is
  specced ahead of confirmation. Mitigated by filing it as an explicit contract
  request and gating implementation on its landing; building against a guessed
  shape is forbidden.
- **R2 — qualname mismatch surfaced as silent miss.** A producer divergence
  (Wardline composing a qualname Clarion can't match) degrades to
  `resolution_confidence: none`. Mitigated by the shared normative fixture and by
  surfacing `none`/`heuristic` counts rather than dropping them.
- **R3 — enrich path inflating query latency.** A per-entity Filigree round-trip
  on every `issues_for` / `orientation_pack`. Mitigated by file-scoped fetch
  (one request per file, reusable across entities in that file) and the
  enrich-only timeout/degrade already in the Filigree client.

## 10. Deliverables (this session)

1. This spec, committed.
2. New `release:1.1` issue — **Flow B: read-time Wardline finding
   reconciliation** — citing this spec, ADR-018, and contracts.md §reconciliation;
   gated on the Filigree findings-read route.
3. Federation contract request for the Filigree findings-read route (a
   `docs/federation/` request note + a Filigree-side issue).
4. Re-scoped `clarion-1f6241b329` + `clarion-22acf15fd7` (Flow A unblock +
   cross-reference).

Any ADR minted from this work (e.g. an ADR-018 amendment pinning the read-time
consume mechanism) is kept under `docs/clarion/adr/` per the editorial
conventions, authored when the decision locks at build time.

## 11. References

- ADR-018 — L7 qualname divergence with Wardline FingerprintEntry (asterisk 2).
- ADR-015 (Revision 2) — Wardline→Filigree native emission.
- `docs/federation/contracts.md` §"Wardline qualname normalization (entity
  reconciliation)" and §"Consumed Filigree route: issue detail (enrichment)".
- `docs/federation/fixtures/wardline-qualname-normalization.json` — normative
  qualname parity vectors.
- Wardline SP4 — Outputs + Loom Integration (2026-05-30) §2, §6, §10.
- `crates/clarion-mcp/src/filigree.rs` — existing enrich-only Filigree client.
