# Clarion — the road to first-class (roadmap & final form)

**Date:** 2026-06-01  
**Status:** Living reference (roadmap; companion to the Loom goal-state case study)  
**Scope:** Clarion's **final form** as a first-class, enterprise-capable code-intelligence
engine — and the staged path to it — given the Loom operating model and invariants settled
across the 2026-06-01 design sessions. Sibling to
`2026-06-01-loom-goal-state-case-study.md` (the suite umbrella) and
`2026-06-01-loom-stable-entity-identity-conformance.md` (the SEI keystone, where Clarion's
conformance obligations and pre-lock requirements are logged — see also Appendix A).

> **The thesis filter governs every line of this roadmap.** "Bring it to enterprise
> level" means enterprise *capability* delivered as **opt-in layers** — never enterprise
> *weight* in the base. The zero-dependency base stays zero-dependency. An agent or team
> that only runs `clarion analyze` and `clarion serve` for MCP navigation pays nothing for
> SEI infrastructure, governance attestations, or the full dossier. Those are layers they
> switch on.

---

## 0. The final form, in one sentence

> Clarion becomes the **best code-intelligence engine in the Loom suite** — the sole
> identity authority, the catalog every agent and tool keys its facts against, the source
> of one-call structural mastery — with a **refactor-stable entity identity** (SEI) that
> makes every cross-tool binding survive the operations developers actually perform, and
> a **full HTTP + MCP read surface** that closes the suite's linkage gap and makes the
> dossier possible.

"First-class" has **two co-equal halves.** The code-intelligence quality bar comes *first*
(analysis depth, MCP completeness, guidance maturity, multi-language breadth); the
suite-integration bar comes *second* (SEI authority, HTTP linkages, dossier participation,
`legis` governance). The design sessions concentrated on the second half — this roadmap
deliberately corrects that imbalance by leading with the first.

---

## 1. Half 1 — code intelligence quality (the foundation; mostly Clarion-autonomous)

This is where "first-class" starts. A Clarion that is a perfect SEI authority but serves
a thin MCP surface or a shallow entity catalog is not first-class. None of this half is
gated on a sibling tool.

### 1.1 Full MCP tool catalogue

The v1.0 MCP surface ships **8 tools** from a catalogue of ~35. The deferred surface
(cursor-based navigation, write-effect inspection, semantic search, guidance authoring,
scope/session management, findings operations, the exploration-elimination shortcuts) is
the primary product surface for consult-mode agents. Closing this gap is the single
highest-leverage autonomous item: it is what agents actually reach for.

- **Navigation**: `goto`, `goto_path`, `back`, `zoom_out`, `zoom_in`, `breadcrumbs`
- **Inspection**: `source`, `metadata`, `guidance_for`, `findings_for`, `wardline_for`
- **Search**: `search_semantic`, `find_by_tag`, `find_by_wardline`, `find_by_kind`
- **Exploration shortcuts**: `find_entry_points`, `find_http_routes`, `find_data_models`,
  `recently_changed`, `high_churn`, `what_tests_this`, `find_circular_imports`, and
  the full catalogue
- **Findings & guidance**: `emit_observation`, `promote_observation`, `propose_guidance`,
  `promote_guidance`
- **Session**: `set_scope_lens`, `session_info`

### 1.2 Guidance system maturity

Guidance is the primary LLM-context-enrichment mechanism. At v1.0 it ships functional but
without the CLI authoring workflow, staleness-driven review prompts, or the
`propose_guidance → observation → promote` lifecycle fully wired into the MCP surface.
First-class means:

- CLI `clarion guidance create/edit/show/list/promote` complete
- Wardline-derived guidance auto-generation stable and tested against real Wardline output
- `CLA-FACT-GUIDANCE-CHURN-STALE` and `CLA-FACT-GUIDANCE-ORPHAN` signals visible to
  operators in a reviewable form
- Guidance import/export for team sharing

### 1.3 Multi-language plugin support

The Python plugin is the v1.0 validating plugin and the reference implementation. First-class
means the plugin manifest protocol is stable enough that a second language (TypeScript,
Go, Rust — prioritised by customer demand) can be built as a first-class peer without
core changes. This requires:

- Plugin manifest protocol stabilised and documented as a public spec (today it is
  implemented but not externally documented)
- Plugin distribution via GitHub Release assets working end-to-end (ADR-033 baseline
  is specified; needs validation with a real third-party plugin)
- `clarion-plugin-fixture` contract exported as the conformance harness new plugin
  authors run

### 1.4 Analysis pipeline: resumability and incremental runs

At v1.0, `clarion analyze` is wipe-and-rerun except at phase-boundary checkpoints. For
large codebases (elspeth target: ~1,100 files, 9-phase pipeline) this is an operational
burden. First-class means:

- File-level incremental re-analysis: skip files whose `content_hash` matches the prior
  run's stored hash (the infrastructure already exists in the content-hash cache; the
  pipeline phase logic needs to consume it)
- Reliable `--resume` across all phases, validated against real interruption scenarios

> **Note:** prior-index retention for the SEI matcher (§2.1) is a prerequisite for
> file-level incremental runs as well — the two requirements share the same storage
> primitive and are sequenced together.

### 1.5 Operational quality

- `clarion doctor` (shipped in v1.1) is the right model; extend it to cover DB health,
  plugin availability, and configuration validation
- Cost estimation accuracy: target the ±50% NFR-COST-03 bound with a validated elspeth
  run; close known overestimation paths (subsystem synthesis cost, prompt-cache hit rates)
- Summary cache staleness signals (`stale_semantic: true` on neighborhood-drift entries)
  visible to operators before they act on outdated briefings

---

## 2. Half 2 — first-class Loom citizen (the layers; mostly gated)

### 2.1 SEI authority — identity minting, retention, matching, and resolution

*(Clarion-autonomous for groundwork; full SEI gated on lock — see Appendix A)*

Clarion is the sole identity authority for the suite. Every cross-tool binding — Wardline
taint facts, Filigree issue associations, `legis` governance attestations — keys on an SEI
that Clarion mints and resolves. Everyone else is a consumer. This is the heaviest Clarion
obligation in the second half.

**Shape-independent groundwork (safe to start before lock):**

- **Prior-index state retention.** Today `clarion analyze` is wipe-and-rerun with no
  retained prior snapshot (`clarion-mcp/src/index_diff.rs`). The matcher needs to diff
  against the previous run's SEI↔locator+body-hash+signature map. A lightweight side
  table retaining the prior-run map is the prerequisite for everything else in this
  section — sequence it first, since it also unblocks file-level incremental analysis
  (§1.4). This is the build item that can and should start before SEI lock.

**After SEI lock:**

- **SEI minting.** On first SEI-aware run, mint a `clarion:eid:<token>` for every entity
  and persist it. The entity ID format (today `{plugin}:{kind}:{qualname}`) is now the
  *locator*; SEI is the identity.
- **Deterministic re-binding matcher.** Three cases: locator unchanged (carry SEI),
  git-rename + identical body (carry SEI, emit `locator_changed`), identical
  body+signature at new module (carry SEI, emit `moved`). Fall through: fail closed —
  mint new SEI, mark old `orphaned`. The git-rename signal requires new surface in the
  analysis pipeline (shell/libgit2; migrates to `legis` git interface when it ships).
- **Lineage log.** Append-only table of SEI events: `born`, `locator_changed`, `moved`,
  `orphaned`, `superseded`. New schema migration.
- **HTTP wire contract.** `resolve(locator)` → current SEI; `resolve_sei(sei)` →
  current locator + alive/orphaned; `lineage(sei)` → event list;
  `_capabilities` advertising `sei: { supported: true, version: N }`.
  Input-validation contract: `resolve(locator)` MUST reject an SEI-shaped string
  (fail-closed; required for safe idempotent backfill — REQ-F-02).
- **Migration cutover.** Coordinate with Filigree and Wardline on the single hard
  cutover; run the backfill that re-keys existing associations from locators to SEIs;
  flag unresolvable orphans for human review.

**Conformance:** proven via the §8 oracle in the SEI spec, not assumed.

### 2.2 HTTP linkages — serving callers/callees over HTTP

*(Clarion-autonomous; co-equal with SEI for the combination matrix)*

Today callers/callees are **MCP-only**. The Wardline + Clarion dossier
(`2026-06-01-wardline-loom-entity-dossier-design.md`) requires linkages over HTTP so
Wardline (and any other HTTP consumer) can pull structural context without running a full
MCP session. This is gated on nothing except Clarion shipping it.

- Add `GET /api/v1/entities/{id}/callers` and `.../callees` (and the batch variants) to
  the existing HTTP read API (ADR-034 defines the transport; these are new endpoints on
  an existing surface)
- Pagination and confidence-tier filtering consistent with the MCP `callers_of` tool
- `_capabilities` advertising `linkages: { http: true }` so consumers can detect
  pre-linkage Clarion and degrade rather than crash
- Once shipped, **this and SEI together close the dossier gate** for the
  Wardline + Clarion combination (goal-state checklist item 3)

### 2.3 The dossier — Clarion's participation in the one-call mastery read

*(Gated on Clarion SEI + Clarion HTTP linkages)*

When SEI and HTTP linkages are both live, Clarion's contribution to the dossier becomes:

- Structural context: entity metadata, containment chain, subsystem membership
- Linkages: callers, callees (both keyed on SEI, served over HTTP)
- Guidance sheets applicable to the entity
- Open Filigree associations (already served via `issues_for`)
- Two-axis freshness status: SEI alive/orphaned + content_hash fresh/stale

Wardline contributes taint posture; Filigree contributes open work. The three pieces
combine in the dossier envelope; Clarion is not the dossier assembler — it is the
structural and identity contributor.

### 2.4 Trust vocabulary convergence and `legis` governance

*(Gated on `legis` existing)*

The suite converges on **one** trust vocabulary — Wardline's grammar delivering
elspeth's effects (custody, fabrication test, fail-closed boundaries) in Loom's own
terms, not elspeth's `tier1/2/3` naming. Clarion's role: carry `declared_tier` and
`wardline_groups` on entities as it does today; the vocabulary these values come from
converges via the suite's trust-vocabulary pass (that pass is not Clarion's to lead).

When `legis` ships, governance attestations key on SEI — Clarion is already the identity
authority, so no new Clarion surface is required. Clarion's `lineage` endpoint is the
audit spine `legis` reads. This is a consumer relationship: `legis` consumes Clarion's
SEI and lineage; Clarion gains nothing new to build.

---

## 3. Staging — by capability milestone and dependency gate

| # | Milestone | Gate | Clarion's position |
|---|---|---|---|
| 1 | **MCP surface completion** — full tool catalogue, exploration shortcuts, guidance MCP lifecycle | none (autonomous) | owns it end-to-end; highest-leverage unblocked item |
| 2 | **Guidance system maturity** — CLI workflow, Wardline-derived guidance, staleness signals | none (autonomous) | owns it end-to-end |
| 3 | **Prior-index state retention** — retained SEI↔locator+body-hash+signature map across re-index | none (autonomous; shape-independent SEI groundwork) | owns it; also unblocks §1.4 incremental analysis |
| 4 | **HTTP linkages** — callers/callees over HTTP read API | none (autonomous) | owns it; co-equal SEI gate for dossier |
| 5 | **SEI authority** — minting, matcher, lineage, wire contract, migration | **SEI lock** (§0.3 of SEI spec) | owns it; gated on lock, not on siblings |
| 6 | **Dossier participation** — structural + identity contribution to the one-call read | Clarion SEI **+** HTTP linkages (§4 above; internal gates only) | closes when 4 + 5 done |
| 7 | **Multi-language plugin** — second-language plugin, manifest protocol published | none (autonomous; milestone 1 sets the protocol) | owns it |
| 8 | **Governance consumption** — SEI lineage as legis audit spine, attestation compatibility | `legis` exists | thin on Clarion's side; waits on sibling |

**Honest gating picture.** Milestones 1–5 are Clarion's to finish alone. Milestones 6
closes with internal gates only (no sibling dependency beyond what Clarion builds).
Milestone 7 is autonomous. Milestone 8 waits on `legis`. The suite's dossier gate
(Wardline milestone 4) waits on Clarion milestones 4 and 5 — and those two are both
**autonomous Clarion work**.

---

## 4. North Star — any entity, any language, any producer

The general form of Clarion's identity: SEI is a property of the *authored thing*, not
of any one language's qualname. At goal state, any entity — function, class, module,
commit, CI artefact, infrastructure resource — describable by any plugin can carry a
SEI and participate in the combination matrix. The Python plugin is the validating
instance. Other languages and other entity kinds are *other producers* following the
same plugin manifest contract — not a rewrite of the core.

The North Star matcher is **edit-tolerant fuzzy** matching: carry SEI across a rename
*with* a body edit, above a high similarity threshold, still fail-closed below it. v1
ships the deterministic subset; fuzzy is the upper bound the design is compatible with.

---

## 5. The throughline

Every item above is an **opt-in layer**. The base is: `clarion analyze` builds the catalog;
`clarion serve` makes it queryable. That is the zero-cost entry point and it must stay
that way. SEI infrastructure, HTTP linkages, the dossier envelope, governance audit —
all are switches a user flips by connecting sibling tools or expanding configuration.
A solo user running MCP navigation over a Python project never pays for suite
infrastructure they did not turn on.

That is enterprise/first-class on Clarion's terms: the richest code-intelligence view
in the suite, delivered without forcing anyone into the suite.

---

## Appendix A — SEI Conformance Position (pre-lock requirements intake)

**Status:** Clarion is the SEI authority. Its §5 obligations are accepted as stated.
This appendix records Clarion's **pre-lock requirements** per SEI spec §0.3 and §0.5.

### Confirmed obligations (§5)

| Obligation | Clarion's position |
|---|---|
| Mint + persist SEI | Confirmed; requires SEI lock first |
| Retain prior-index state (§3.1) | Confirmed; shape-independent groundwork starts now |
| Run deterministic matcher | Confirmed; signals discussion below |
| Fail-closed mint + lineage on ambiguity | Confirmed; consistent with existing no-false-green posture |
| Serve `resolve` / `resolve_sei` / `lineage` | Confirmed; HTTP read API is the natural home (ADR-034) |
| Advertise `sei` capability + version | Confirmed; `_capabilities` endpoint extension |

### Concrete emerging requirements (pre-lock input)

**REQ-C-01 — "Signature" needs a formal definition. → RESOLVED in ADR-038 (2026-06-02).**  
The matcher's move case ("identical body hash and identical signature at a new module")
depends on a "signature" that Clarion does not currently store as a discrete field.
**Resolution:** a plugin-declared, versioned JSON object stored verbatim in a plain (non-unique)
`entities.signature TEXT`, compared by string equality; `null` where comparison is not meaningful.
Caveat recorded in ADR-038: signature is near-redundant for the *v1 deterministic* move case
(byte-identical body already implies identical signature) — it is carried for spec-conformance and
as the load-bearing input to the North-Star fuzzy matcher. See the integrated delivery plan
(`docs/superpowers/plans/2026-06-02-clarion-integrated-delivery-plan.md`) REQ-C-01.

**REQ-C-02 — SEI token scheme. → RESOLVED in ADR-038 (2026-06-02), correcting this entry.**  
This entry originally preferred a content-addressed `blake3(locator-at-birth)`. **A peer review
against the code found that wrong:** (1) the entity field it would key on (`first_seen_commit`) is
**never populated** by the pipeline, so the token degenerates to `blake3(locator)` — the
collision-on-reuse flaw; and (2) the "make the token a pure function to preserve determinism" frame
is itself the error — **SEI allocation is stateful** (the matcher carries-or-mints against the
persisted binding store), so reproducibility of the SEI *value* comes from the `sei_bindings` table,
not from re-deriving the token. **Resolution:** `clarion:eid:<blake3(locator ++ 0x00 ++
mint_run_id)[:32 hex]>` where `mint_run_id` is the minting run's UUID — collision-free under locator
reuse (a reused locator is only ever minted, in a later run), no time/RNG component. The
byte-identical-run guarantee covers entity/edge/finding *state*, explicitly **not** identity values.
See the integrated delivery plan REQ-C-02 and ADR-038.

**REQ-C-03 — Prior-index retention scope to be scoped as a side table, not a full snapshot.**  
The matcher needs "the prior SEI↔locator + body-hash + signature map." This is a
narrow, keyed map — not a full DB snapshot or a full prior `clarion.db`. The right
storage is a lightweight side table (migration M4 or M5) that persists across re-index
and is cleared only on explicit `--force` reinit. Clarion records this to prevent the
requirement from being interpreted as "keep the full prior DB around," which would
significantly change storage cost and the git-committable posture. **Clarion flags this
as a design constraint, not a blocker.**

**REQ-C-04 — MCP surface treatment needs a conformance obligation.**  
SEI spec §4 defines the wire contract as HTTP endpoints. Clarion's MCP tools
(`find_entity`, `entity_at`, `callers_of`, `neighborhood`, etc.) also return entity
ids today. After SEI, the question is: should MCP responses carry SEI, locator, or both?
If a consumer (e.g. a Wardline MCP-mode session) receives a locator from Clarion's MCP
surface and uses it as a cross-tool binding key, the SEI invariant is broken regardless
of what the HTTP surface does. **Clarion requests that §5 be extended to cover the MCP
surface, or that the spec explicitly limits the conformance obligation to the HTTP
surface and documents the MCP locator exception.**

**REQ-C-05 — Git-rename signal: source and surface.**  
v1 sources git-rename detection in Clarion (shell/libgit2). The analysis pipeline
currently reads git metadata for `first_seen_commit`/`last_seen_commit` via
`git log --follow`-equivalent calls during Phase 1.5. Extending this to detect
renames is feasible but requires new code surface. If `legis` eventually owns the git
interface and supplies this signal, the matcher should consume "a git-rename signal
interface" from the start rather than `Clarion::git_rename()` directly. **Clarion
notes this as an interface boundary to define cleanly in v1, even if `legis` is not yet
built.**

### Non-requirements (Clarion's scope boundary)

Clarion is **not** the dossier assembler. The dossier envelope is produced by the
consumer (Wardline in the current design) by calling Clarion's HTTP surface. Clarion
does not aggregate Wardline taint facts or Filigree issues — it contributes its slice
and the consumer composes. This is consistent with the enrichment-not-load-bearing
axiom and Clarion's existing enrich-only posture.

Clarion is **not** the trust vocabulary convergence lead. Wardline owns the grammar;
`legis` governs it. Clarion carries `declared_tier` and `wardline_groups` verbatim and
updates them when Wardline's schema updates. Clarion does not adjudicate trust.
