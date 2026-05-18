# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository state

**Sprint 1 is closed.** The walking skeleton is tagged at `v0.1-sprint-1` (merge commit `48b9bb0` on `main`). All nine Sprint-1 lock-ins (L1–L9) are ratified; `clarion analyze` end-to-end persists `python:function:demo.hello|function` against a fixture project. See `docs/implementation/sprint-1/signoffs.md` for the closed Tier-A ladder.

### Layout (post-Sprint-1)

- **Rust workspace** at repo root (`Cargo.toml`, `crates/`):
  - `crates/clarion-core/` — entity-ID assembler, plugin host (`plugin/host.rs`), JSON-RPC transport, manifest parser, jail + limits, discovery, breaker.
  - `crates/clarion-storage/` — writer-actor + reader-pool over SQLite (per ADR-011).
  - `crates/clarion-cli/` — the `clarion` binary; `install` and `analyze` subcommands.
  - `crates/clarion-plugin-fixture/` — test-only fixture plugin used by `wp2_e2e` integration tests.
- **Python plugin** at `plugins/python/` (editable install: `pip install -e plugins/python[dev]`). Speaks the L4 JSON-RPC protocol; emits function entities with L7 qualnames; runs the L8 Wardline probe.
- **Shared cross-language fixture** at `fixtures/entity_id.json` — the L2 byte-for-byte parity proof (consumed by Rust + Python tests both).
- **End-to-end test** at `tests/e2e/sprint_1_walking_skeleton.sh` — runs the README §3 demo verbatim and asserts the sqlite output.
- **CI** at `.github/workflows/ci.yml` — three jobs: `rust` (fmt, clippy `-D warnings`, nextest, doc, deny), `python-plugin` (ruff, ruff-format check, mypy --strict, pytest), `walking-skeleton` (depends on the first two; runs the e2e script).

### Build / test commands

ADR-023 names these as the floor — every PR must pass all of them.

```bash
# Rust gates
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins        # wp2_e2e tests need clarion-plugin-fixture on disk
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check

# Python gates (run from repo root)
plugins/python/.venv/bin/ruff check plugins/python
plugins/python/.venv/bin/ruff format --check plugins/python
plugins/python/.venv/bin/mypy --strict plugins/python
plugins/python/.venv/bin/pytest plugins/python

# End-to-end
bash tests/e2e/sprint_1_walking_skeleton.sh
```

Pre-commit hooks at `.pre-commit-config.yaml` (repo root) wire ruff + ruff-format + mypy on every `git commit`. Install with `plugins/python/.venv/bin/pre-commit install`.

The Sprint-1 demo script in `docs/implementation/sprint-1/README.md` §3 is the canonical first-build recipe and is verified in CI by the `walking-skeleton` job.

## What Clarion is, in one paragraph

Clarion is a code-archaeology tool: it ingests a codebase, extracts entities (functions, classes, modules), clusters them into subsystems, and serves structured briefings to consult-mode LLM agents over MCP so those agents do not have to re-explore the tree on every question. Single-binary Rust core + language plugins (Python first); SQLite-backed local state under `.clarion/`; designed for "enterprise rigor at lack of scale." Target first customer is `elspeth` (~425k LOC Python).

Clarion is one of three (soon four) products in the **Loom** suite. The other repos — `filigree` and `wardline` — are not vendored here but are owned by the same author and are referenced extensively. Cross-product work in WP9/WP10/Sprint-2+ is within-scope, not external.

## Doctrine you must read before changing design docs

The Loom federation axiom in `docs/suite/loom.md` (especially §3–§5) is **load-bearing for every architectural decision in this repo**. The three rules:

1. Each product is solo-useful.
2. Each pair composes meaningfully on its own.
3. Integration is enrich-only — a sibling may add information to another product's view but must never be required for that product's semantics to make sense.

Before proposing or accepting any change that adds a new dependency, "lightweight glue layer," shared registry, or cross-product mediator, run it against the §5 failure test (semantic / initialization / pipeline coupling). Centralisation creeps back in naturally; treat any "wouldn't it be easier if we just..." proposal as suspicious.

Two named v0.1 asterisks (Wardline→Filigree pipeline coupling via Clarion; Python plugin's `wardline.core.registry.REGISTRY` import) have written retirement conditions in `loom.md` §5. Do not add new asterisks without the same.

## Documentation map

```
docs/
├── suite/                         Loom-wide doctrine (read-first for new contributors)
│   ├── briefing.md                5-minute introduction
│   └── loom.md                    Founding doctrine, federation axiom, go/no-go test
├── clarion/
│   ├── v0.1/                      Canonical product docset for the current version
│   │   ├── requirements.md        The WHAT — REQ-/NFR-/CON-/NG- IDs, baselined
│   │   ├── system-design.md       The HOW — architecture, mechanisms, §2–§11 with `Addresses:` headers
│   │   ├── detailed-design.md     Implementation reference — schemas, rule catalogs, appendices
│   │   ├── plans/                 Scope memo (v0.1-scope-commitments.md)
│   │   └── reviews/               Retained historical reviews (supporting context, not normative)
│   └── adr/                       Authored architecture decision records (ADR-001 … ADR-022)
└── implementation/                Work-package sequencing (lives ABOVE v0.1/ because WPs span siblings)
    ├── v0.1-plan.md               11 WPs in dependency order, with anchoring docs/ADRs per WP
    └── sprint-1/                  Per-sprint execution plan (walking-skeleton: WP1+WP2+WP3)
```

### Reading order by intent

- **New to the project**: `docs/suite/briefing.md` → `docs/suite/loom.md` → `docs/clarion/v0.1/README.md`.
- **Implementing**: `requirements.md` → `system-design.md` → `detailed-design.md` → relevant ADRs → the WP doc under `docs/implementation/`.
- **Reviewing a design proposal**: read the requirement IDs it cites, then the system-design section listed in those requirements' `See` lines, then check whether any Accepted ADR already constrains the answer.

## Where canonical truth lives

When the same fact appears in multiple files, this is the precedence:

1. **Accepted ADRs** in `docs/clarion/adr/` — the locked decisions. 16 are Accepted at v0.1; six remain Backlog and are tracked inside `system-design.md` §12 / `detailed-design.md` §11 until promoted.
2. **`requirements.md`** — REQ-/NFR-/CON-/NG- IDs are stable and load-bearing (filigree issues and commit messages cite them by ID; never reuse a retired ID).
3. **`system-design.md`** — `Addresses:` headers on each §2–§11 section define the requirement acceptance surface for that subsystem.
4. **`detailed-design.md`** — exact schemas, rule catalogues, appendices.
5. Reviews under `docs/clarion/v0.1/reviews/` are supporting context only, not normative. Do not cite a review as the source of a current decision; cite the ADR or design doc that absorbed it.

If `requirements.md` and `system-design.md` disagree, the requirement wins and the design doc is the bug. If an ADR exists, it overrides both.

## Implementation work-package vocabulary

Work is organised as numbered Work Packages (WP1–WP11) and grouped into sprints. Each WP doc has the same skeleton: scope, deliverables, exit criteria, anchoring system-design sections, ADRs satisfied, ADRs surfaced, unresolved questions.

Sprint 1 commits a numbered set of "lock-ins" (L1–L9) — design surfaces that are cheap to change before the sprint closes and expensive after. When touching anything in `wp1-scaffold.md`, `wp2-plugin-host.md`, or `wp3-python-plugin.md`, check the lock-in table in `docs/implementation/sprint-1/README.md` §4 first; later sprints will read and write against those exact shapes.

## Key terminology to use consistently

- **Entity ID** (per ADR-003 + ADR-022): three colon-separated segments — `{plugin_id}:{kind}:{canonical_qualified_name}`, e.g. `python:function:auth.tokens.refresh`. The plugin owns segments 1 and 3; the core never invents kinds.
- **Finding**: a unified record type for defects, structural observations, classifications, metrics, and suggestions — emitted by Clarion (and other Loom tools) into Filigree via `POST /api/v1/scan-results`. See ADR-004.
- **Observation**: fire-and-forget agent note (see Filigree workflow). Distinct from a Finding.
- **Guidance sheet**: institutional knowledge attached to an entity (Clarion-authored).
- **Briefing**: structured per-entity summary that Clarion serves to consult-mode agents.
- **Loom suite**: the federation. Refer to it as "the Loom suite" in docs (per project memory). Member products are Clarion, Filigree, Wardline, and the proposed Shuttle.

Avoid: "Loom platform," "Loom runtime," "Loom broker," "Loom store" — Loom is a family name and a doctrine, not anything that runs (per `loom.md` §6).

## Editorial conventions for design docs

- ADR files are immutable once Accepted, except for status changes and "Superseded by ADR-NNN" links. To revise an Accepted ADR, write a new ADR that supersedes it.
- Each requirement statement has: stable ID, plain-English statement, rationale, verification method, and a `See:` link to the addressing system-design section. Match the existing pattern when adding requirements.
- When renaming or moving design files, prefer `git mv` over leaving redirect stubs behind. The user has explicitly rejected legacy-filename "history preservation" tech debt.

## Task tracking

`filigree` is the issue tracker for this project (config in `.filigree/`, MCP server registered in `.mcp.json`). The global `~/CLAUDE.md` file describes the workflow and CLI/MCP commands; do not duplicate that here. Project-specific notes:

- Sprint 1 issues (`WP1`, `WP2`, `WP3`, Sprint-1-close) are all `delivered`/`closed`. Sprint 2 issues should follow the same `release:v0.1`, `sprint:N`, `wp:N`, `adr:NNN` label scheme.
- Filigree issue bodies should cite `REQ-*` / `NFR-*` / ADR IDs verbatim — those IDs are how design docs and tracker stay linked.

### Live carryover from Sprint 1

These were filed during Sprint 1 with explicit deferral rationale and are ready for Sprint 2 triage:

- `clarion-889200006a` (P3, sprint:2 / wp:9) — ADR-018 amendment: L7 qualname divergence with Wardline `FingerprintEntry`. Triggers when WP9 attempts the first cross-product join.
- WP2 deferred items: `clarion-48c5d06578` (supervisor drain/discard), `clarion-928349b60f` (jail TOCTOU on briefing read), `clarion-35688034f0` (read_frame deadline), `clarion-c0977ac293` (RLIMIT_AS observed end-to-end), `clarion-adeff0916d` (fixture-binary self-build).
- WP1 review-2 P2 bugs: `clarion-5e03cfdd21`, `clarion-ed5017139f`, `clarion-b5b1029f5a`, `clarion-4cd11905e2` — good Sprint-2 warm-up before Tier-B feature work.

<!-- filigree:instructions:v2.0.3:d454f2c2 -->
## Filigree Issue Tracker

`filigree` tracks tasks for this project. Data lives in `.filigree/`. Prefer
the MCP tools (`mcp__filigree__*`) when available; fall back to the `filigree`
CLI otherwise.

### Workflow

```bash
# At session start
filigree session-context                            # ready / in-progress / critical path

# Pick up the next ready issue (atomic claim + transition to in_progress)
filigree start-next-work --assignee <name>
# ...or claim a specific issue
filigree start-work <id> --assignee <name>

# Do the work, commit, then
filigree close <id>
```

Use the atomic claim+transition verbs — `start_work` / `start_next_work`
(MCP) or `start-work` / `start-next-work` (CLI). Do **not** chain
`claim_issue` (MCP) or `filigree claim` (CLI) with a subsequent status
update — the two-step form races against other agents; the combined verb is
atomic.

### Observations: when (and when not) to use them

`observe` is a fire-and-forget scratchpad for *incidental* defects — things
you notice *outside the scope of your current task* (a code smell in a
neighbouring file, a stale TODO, a missing test for an edge case you happened
to spot). Notes expire after 14 days unless promoted. Include `file_path` and
`line` when relevant. At session end, skim `list_observations` and either
`dismiss_observation` or `promote_observation` for what has accumulated.

**You fix bugs in your currently defined scope. You do NOT use observations
to finish work prematurely.** If a defect, gap, or follow-up belongs to your
current task, you own it — handle it as part of that task: fix it now, expand
the task's scope, file a proper issue with a dependency, or surface it to the
user. Filing it as an observation and closing the task is *not* completing
the task; it is shipping known-broken work and hiding the debt in a 14-day
expiring scratchpad. The test is "would I have noticed this even if I weren't
working on this task?" If no, it's task scope, not an observation.

### Priority scale

- P0: Critical (drop everything)
- P1: High (do next)
- P2: Medium (default)
- P3: Low
- P4: Backlog

### Reaching for tools

MCP tool schemas describe each tool; `filigree --help` and `filigree <verb>
--help` are the authoritative CLI reference. You do not need to memorise
either catalogue. The verbs you will reach for most:

- **Find work:** `get_ready`, `get_blocked`, `list_issues`, `search_issues`
- **Claim work:** `start_work`, `start_next_work`
- **Update:** `add_comment`, `add_label`, `update_issue`, `close_issue`
- **Scratchpad:** `observe`, `list_observations`, `promote_observation`, `dismiss_observation`
- **Cross-product entity bindings (ADR-029):** `add_entity_association`,
  `remove_entity_association`, `list_entity_associations`,
  `list_associations_by_entity`. Used when a sibling tool (e.g.
  Clarion) needs to bind a Filigree issue to a function, class, or
  module identifier it owns. The `entity_id` is an opaque string
  from Filigree's perspective; the consumer (the sibling tool's read
  path) does drift detection against the stored
  `content_hash_at_attach`. `list_associations_by_entity` is the
  reverse-lookup surface — given a Clarion entity ID, return every
  Filigree issue bound to it (project isolation is by DB file). Also
  reachable over HTTP as
  `GET/POST /api/issue/{issue_id}/entity-associations`,
  `DELETE /api/issue/{issue_id}/entity-associations?entity_id=…`,
  and `GET /api/entity-associations?entity_id=…`.
- **Health:** `get_stats`, `get_metrics`, `get_mcp_status`

Pass `--actor <name>` (CLI) so events attribute to your agent identity.

### Error handling

Errors return `{error: str, code: ErrorCode, details?: dict}`. Switch on
`code`, not on message text. Codes: `VALIDATION`, `NOT_FOUND`, `CONFLICT`,
`INVALID_TRANSITION`, `PERMISSION`, `NOT_INITIALIZED`, `IO`,
`INVALID_API_URL`, `STOP_FAILED`, `SCHEMA_MISMATCH`, `INTERNAL`.

On `INVALID_TRANSITION`, call `get_valid_transitions` (MCP) or
`filigree transitions <id>` to see what the workflow allows from here.

Two failure modes deserve a specific response:

- **`SCHEMA_MISMATCH`** — the installed `filigree` is older than the project
  database. The error message contains upgrade guidance. Surface it to the
  user; do not retry.
- **`ForeignDatabaseError`** — filigree found a parent project's database
  but no local `.filigree.conf`. Run `filigree init` in the current
  directory. Do **not** `cd` upward to a different project unless that was
  the actual intent.
<!-- /filigree:instructions -->
