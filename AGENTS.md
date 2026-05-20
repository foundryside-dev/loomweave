# AGENTS.md

This file is the operating contract for coding agents working in this
repository. It is intentionally practical: start here, then follow the linked
source-of-truth docs when the task touches design, release, or federation
semantics.

## First Principles

Clarion is a local-first code-archaeology tool. It ingests a repository,
extracts entities and relationships, stores the graph in SQLite, and serves
that graph to consult-mode agents over MCP and the federation HTTP read API.
The v1.0 product is a Rust workspace plus a Python language plugin.

Clarion is part of the Loom suite with Filigree and Wardline. The governing
doctrine is the Loom federation rule:

1. Each product must remain useful alone.
2. Each pair of products must compose meaningfully.
3. Integration must enrich a product's view, not become required for that
   product's semantics to make sense.

Before accepting any design that adds a shared runtime, shared registry,
cross-product mediator, mandatory sibling dependency, or "small glue layer,"
read [docs/suite/loom.md](docs/suite/loom.md) and apply its failure test.
Centralization is usually the thing trying to sneak in wearing a helpful hat.

## Start Every Session

Run a live project-state check before substantive work:

```bash
filigree session-context
git status --short --branch
```

Use the tracker state, current branch, and dirty tree you actually see. Do not
assume a previous handoff, memory entry, or branch name is current.

If the tree is dirty, treat existing changes as user work unless you made them
in this session. Read affected files before editing near them, and do not
revert unrelated changes.

## Source Of Truth

When documents disagree, use this precedence:

1. Accepted ADRs under [docs/clarion/adr/](docs/clarion/adr/).
2. [docs/clarion/1.0/requirements.md](docs/clarion/1.0/requirements.md).
3. [docs/clarion/1.0/system-design.md](docs/clarion/1.0/system-design.md).
4. [docs/clarion/1.0/detailed-design.md](docs/clarion/1.0/detailed-design.md).
5. Implementation and review history under [docs/implementation/](docs/implementation/).

ADRs are decision records, not suggestions. Accepted ADRs are immutable except
for status changes and supersession links. If an accepted ADR is wrong, write or
propose a successor ADR rather than silently rewriting the old decision.

Requirement IDs, ADR IDs, and federation contract names are load-bearing. Keep
them stable and cite them in Filigree issues, commits, and review notes when
they explain the work.

## Reading Map

Use the smallest set that grounds the task:

- New orientation: [README.md](README.md), then
  [docs/suite/briefing.md](docs/suite/briefing.md), then
  [docs/suite/loom.md](docs/suite/loom.md).
- Product design: [docs/clarion/1.0/README.md](docs/clarion/1.0/README.md),
  then requirements, system design, and detailed design in that order.
- Federation work: [docs/federation/contracts.md](docs/federation/contracts.md)
  and the fixtures under [docs/federation/fixtures/](docs/federation/fixtures/).
- Release work:
  [docs/operator/v1.0-release-governance.md](docs/operator/v1.0-release-governance.md),
  [.github/workflows/ci.yml](.github/workflows/ci.yml), and
  [.github/workflows/release.yml](.github/workflows/release.yml).
- Historical sprint context:
  [docs/implementation/README.md](docs/implementation/README.md). Treat this
  as supporting context, not as a normative source.

## Repository Shape

- [Cargo.toml](Cargo.toml) defines the Rust 2024 workspace and shared
  dependency/lint policy.
- [crates/clarion-core/](crates/clarion-core/) owns entity IDs, plugin hosting,
  manifests, process limits, discovery, and core protocols.
- [crates/clarion-storage/](crates/clarion-storage/) owns SQLite storage,
  writer-actor behavior, and reader-pool access.
- [crates/clarion-cli/](crates/clarion-cli/) owns the `clarion` binary and user
  commands such as `install`, `analyze`, and `serve`.
- [crates/clarion-mcp/](crates/clarion-mcp/) owns the MCP consult surface.
- [crates/clarion-scanner/](crates/clarion-scanner/) owns pre-ingest secret
  scanning.
- [crates/clarion-plugin-fixture/](crates/clarion-plugin-fixture/) is a test
  fixture crate.
- [plugins/python/](plugins/python/) is the v1.0 Python language plugin.

Prefer existing boundaries. If a change crosses crate, plugin, CLI, MCP,
storage, or federation boundaries, name the contract being changed and add a
test at the boundary.

## Engineering Rules

Do not guess. Read the source, reproduce behavior when useful, and make claims
only as strong as the evidence you have.

Prefer the earliest boundary that can enforce an invariant. Configuration,
manifest parsing, protocol decoding, storage writes, and public response types
are better places for hard failures than downstream call sites.

For bug fixes, add or identify a focused failing regression before the fix when
the behavior is testable. Keep fixes small enough that the regression proves the
point.

Use structured parsers and APIs where the repo already has them. Avoid ad hoc
string surgery for TOML, JSON, YAML, SQL, or protocol payloads unless the local
code already uses that pattern for the same reason.

Do not introduce new cross-product coupling to make an implementation easier.
If a sibling integration becomes necessary for semantics, stop and reconcile the
design with the Loom doctrine before coding.

Use focused subagents when they materially improve confidence or throughput.
For release reviews, broad audits, multi-surface debugging, and independent
implementation slices, split the work by boundary and dispatch subagents without
asking for another permission round. Keep each subagent prompt self-contained,
give it a narrow scope, avoid overlapping write sets, and integrate its findings
against the live tree before reporting or closing work.

## Verification Gates

ADR-023 sets the CI floor. Run the narrowest useful gate while iterating, then
run the relevant full gate before claiming completion.

Rust gates:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
```

Python plugin gates, from the repo root:

```bash
plugins/python/.venv/bin/ruff check plugins/python
plugins/python/.venv/bin/ruff format --check plugins/python
plugins/python/.venv/bin/mypy --strict plugins/python
plugins/python/.venv/bin/pytest plugins/python
```

End-to-end gates:

```bash
bash tests/e2e/sprint_1_walking_skeleton.sh
bash tests/e2e/sprint_2_mcp_surface.sh
bash tests/e2e/phase3_subsystems.sh
```

For release work, also run the governance guard described in
[docs/operator/v1.0-release-governance.md](docs/operator/v1.0-release-governance.md).
Live GitHub checks need a token with repository administration and Actions
policy read access.

## Filigree Workflow

Filigree is the project tracker. Prefer MCP tools when available; use the CLI
otherwise. Start from live state:

```bash
filigree session-context
```

Use atomic claim-and-transition verbs:

```bash
filigree start-next-work --assignee <name>
filigree start-work <id> --assignee <name>
```

Do not chain `filigree claim` with a later status update. The combined verbs
exist to avoid racing other agents.

Close tracker work only after the implementation, verification, and comments
match reality. If a discovered defect belongs to the active task, own it: fix
it, broaden the task, file a proper dependent issue, or raise the blocker. Do
not hide in-scope work in an expiring observation.

Use observations only for incidental findings outside the current task. At
session end, check accumulated observations and either promote or dismiss what
is ready to triage.

## Release Work

Clarion v1.0 publishes from GitHub Releases. Do not cut tags or treat a commit
as release-ready until:

- Filigree shows no unresolved release blockers.
- The full CI floor has passed on the release commit or PR.
- The GitHub release governance guard passes for `main`.
- The release workflow dry run from `main` passes.
- The public artifact smoke test has been run from the GitHub Release artifacts.

Release notes and status summaries must distinguish local commits, pushed
branches, open PRs, merged PRs, and published tags. A local green branch is not
a shipped release.

## Git And Commit Hygiene

Keep commits scoped to the work requested. Do not stage unrelated dirty files.
If the user asks for a broad commit, re-check `git status --short` immediately
before staging so the scope is explicit.

Prefer normal, reviewable history. Do not force-push, reset hard, delete
branches, or discard work unless the user explicitly asks and the destructive
scope is clear.

If hooks fail, fix the blocker rather than bypassing hooks unless the user
explicitly authorizes a checkpoint with failing gates.

## Communication

Keep status updates short and concrete: what you checked, what changed, what is
blocked, and which verification ran. When reporting failures, include the exact
command and the first useful error, not a vague "tests failed."

When you are unsure, say what evidence is missing and go get it if it is cheap.
When evidence is expensive or requires credentials, explain the limitation and
the next command a maintainer can run.

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
