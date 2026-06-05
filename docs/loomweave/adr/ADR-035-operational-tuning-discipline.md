# ADR-035: Operational Tuning Discipline — Declared Basis, Override Surface, Retune Trigger, Coupling

**Status**: Accepted
**Date**: 2026-05-23
**Deciders**: qacona@gmail.com
**Context**: From-scratch architecture review on 2026-05-22 (`docs/arch-analysis-2026-05-22-1924/`) surfaced five open questions in `04-final-report.md` §8 that a follow-on SME roundtable (solution architect, systems thinker, Python engineer, quality engineer, security engineer) reframed as a single missing-feedback-loop pattern. This ADR is the level-5 (rules) intervention that pattern requires.
**Extends**: [ADR-021](./ADR-021-plugin-authority-hybrid.md) §4 — names four of the eleven operational constants as config keys; this ADR generalises the discipline that ADR-021 §4 already implies for those four and extends it to every operational constant in the workspace.

## Summary

Loomweave ships with strong **runtime** balancing loops — the path-escape breaker, the crash-loop breaker, the writer-actor's `parent_contains_mismatch` bijection check, the L2 cross-language fixture parity test — and no **design-time** balancing loops. The result is silent drift: hardcoded operational constants accrete without recorded basis, crate-level doc-comments diverge from contents, file LOC grows past the point of legibility, and the artifact that would name the right tuning surface (a config schema, a split plan, a crate boundary) is never written. The five §8 open questions are five surface symptoms of one pattern.

This ADR commits the project to a uniform discipline. Every operational constant in Loomweave source that gates externally observable behaviour MUST declare four things — **stated basis**, **override surface**, **retune trigger**, **coupling** — in a code comment immediately adjacent to the constant (or, for wire-contract constants whose declaration lives in a sibling document, a cross-reference to that document). The same rule shape extends to **file-LOC budgets** (any source file over 1,500 LOC declares a split-trigger condition in its module doc-comment) and to **crate-boundary budgets** (any crate that takes a dependency widening its trust surface or contradicting its `lib.rs` doc-comment declares the trigger for crate extraction). The discipline is enforced by a lint script in `scripts/` that scans Rust + Python sources for the required declarations and fails CI on undeclared operational constants or oversize files.

This is a level-5 (rules) Meadows intervention — not a level-12 (parameters) one. A `loomweave.yaml` config file alone is rejected as insufficient: a config file without the rule that gates how constants graduate from hardcoded → tunable → deprecated would, in the systems thinker's words, "decay back to hardcoded constants by the next sprint — that is exactly how Loomweave got here."

## Context

### The five §8 open questions

The 2026-05-22 architecture analysis (`docs/arch-analysis-2026-05-22-1924/04-final-report.md` §8) recorded five questions that the from-scratch catalog could not answer from code alone:

1. **Why `MAX_FILES_PER_PYRIGHT_SESSION = 25`?** Empirical? Conservative bound on Pyright memory growth? Not knowing makes future tuning a guess.
2. **What is the post-1.0 plan for the four monolith files?** Each has a natural refactor split. Are these on the roadmap, or is the policy "no split until the file actively impedes a change"?
3. **Will `loomweave-llm` become a crate?** The `llm_provider.rs` placement in `loomweave-core` is the largest single argument against the `lib.rs:1` doc-comment.
4. **What is the architect's stance on `application_id` / `user_version`?** Trivial to add; non-trivial to add retroactively once installed DBs exist in the wild.
5. **Operational tuning roadmap.** Eleven hardcoded limits, plus the 25-file restart, plus the 256/50 batch-cadence constants. WP6 is named in code comments — what is its current status?

### The roundtable's diagnosis

Five SME reports (archived under `docs/archive/working-notes/arch-analysis-2026-05-22-1924/answer-{solution-architect,systems-thinker,python-engineer,quality-engineer,security-engineer}.md`) converged on a single root cause. The systems thinker named it most directly:

> "The five questions look like five concerns. They are one. Each is a missing-feedback-loop symptom: a place where operational reality has no path back to the artifact that would change behavior. They wear 'parameter' clothing (Level 12) but live at Level 6 (information flows) and Level 5 (rules)." — `answer-systems-thinker.md`

The archetype is Meadows' **Drift to Low Performance**: each individual deferral is locally rational ("ship 1.0; tune later"; "don't split `host.rs` mid-sprint"; "PRAGMAs are post-1.0") but there is no countervailing signal pushing the other way. Two literal tells in the codebase make the diagnosis concrete:

- **`crates/loomweave-cli/src/analyze.rs:65`** carries `#[allow(clippy::too_many_lines)]` — the standard-lowering act made literal in code. Two more sites at lines 650 and 1190 carry the same allow.
- **`crates/loomweave-core/src/plugin/breaker.rs:7`** says: *"Sprint 1 hard-codes the threshold and window per UQ-WP2-10; the config surface (`loomweave.yaml:plugin_limits.crash_*`) lands in WP6."* The placeholder where a discipline should be.

ADR-030 narrowed WP6's 1.0 scope to the on-demand `summary(id)` MCP tool. The operator-tunables work the `breaker.rs:7` comment references **was not folded into the narrowed WP6 scope and is currently un-homed** (solution architect's reading: `answer-solution-architect.md` §5). ADR-021 §4 names four of the eleven constants as config keys (`plugin_limits.max_frame_bytes`, `plugin_limits.max_records_per_run`, `plugin_limits.max_rss_mib`, the manifest-declared `expected_max_rss_mb`) — those are *promised by an Accepted ADR and not implemented*, with no governing rule about how the other seven graduate from hardcoded to tunable.

### Per-SME contributions to the diagnosis

The five SME reports each examined a different facet of the same pattern:

- **Solution architect** (`answer-solution-architect.md`): triangulates the WP6 home from ADR-021 §4 + ADR-030 + the 2026-04-19 WP2 handoff doc; recommends shipping 1.0 with limits hardcoded and landing the config surface in WP6 post-1.0 *as one ADR-021-aligned change rather than dripping per-constant overrides*. The "one ADR-aligned change" framing is what this ADR operationalises.
- **Systems thinker** (`answer-systems-thinker.md`): identifies Level 5 (rules) as the correct leverage point and rejects the level-12 (parameters) intervention of "just add `loomweave.yaml`" — the rule about how/when constants graduate is the discipline, not the constants themselves. Names the "drift to low performance" archetype.
- **Python engineer** (`answer-python-engineer.md`): classifies the Python-side constants into wire-contract-pinned (must track Rust counterparts; `MAX_CONTENT_LENGTH = 8 MiB` in `server.py:48` mirrors `ContentLengthCeiling::DEFAULT`; `MAX_UNRESOLVED_CALLEE_EXPR_BYTES = 512` in `pyright_session.py:43` mirrors a same-named Rust constant; `STDERR_TAIL_LIMIT = 65536` in `pyright_session.py:49` mirrors `STDERR_TAIL_BYTES = 64 KiB` in `host.rs`) versus operational tunables (six Pyright-session constants). **None carry a comment naming the Rust counterpart.** This forced the fourth declaration axis — *coupling* — into this ADR's rule shape: without it, a wire-pinned constant and a freely-tunable one get the same declaration form, which obscures the more dangerous case.
- **Quality engineer** (`answer-quality-engineer.md`): enumerates per-constant test coverage (twelve constants — eight Tested, three Weak, one Untested, one Partially tested). The Weak/Untested cluster is `DEFAULT_MAX_RSS_MIB`/`DEFAULT_MAX_NOFILE`/`DEFAULT_MAX_NPROC` — security-enforcement constants whose behavioural coverage is "does not panic." A value change here has no regression net. The discipline this ADR establishes interlocks with that gap: a constant whose retune trigger is named is a constant whose regression test is also nameable.
- **Security engineer** (`answer-security-engineer.md`): names Q5 as "the question with the most teeth" from a STRIDE-D + STRIDE-E perspective. The 11+ values include the entity cap (500k), Content-Length ceiling (8 MiB), path-escape breaker threshold (10/60s), `RLIMIT_AS` (2 GiB), `RLIMIT_NOFILE` (256), `RLIMIT_NPROC` (32), HTTP body limit (16 KiB), concurrency limit (64), request timeout (10s), batch maxima (256/1000), and the Pyright restart. Recompile-to-tune is itself a security posture stance — an operator under active adversarial-plugin pressure cannot tighten the breaker threshold without a rebuild. The recommendation: *at v1.0 these are deliberately frozen so the security policy is uniform across deployments; post-1.0, the path-escape breaker threshold, the entity cap, and the `RLIMIT_AS` ceiling should become operator-tunable with hard floors enforced at config-load time.* That stance survives intact in this ADR's "internal, no override" allowed state.

### What this ADR is not

This ADR is **not** a tracking surface in Filigree, an entity-association registry, or a derived-metric dashboard. It is a source-comment + lint-script discipline, period. ADR-029's entity-association binding is a different mechanism for a different problem; the two do not overlap. This ADR also does not invent a `loomweave.yaml` field set — ADR-021 §4 already names four of the eleven config keys, and the per-constant override-surface placement is the matter of subsequent WPs (post-1.0 WP6 by the solution architect's recommendation). What this ADR commits is the **rule** that gates whether a constant is declared, where its override surface lives, and what would prompt its retune.

## Decision

### 1. The four-axis declaration rule

Every operational constant in Loomweave source that gates externally observable behaviour MUST declare four things, either in a code comment immediately adjacent to the constant or in a sibling doc cross-reference if the constant's authoritative declaration lives elsewhere:

1. **Stated basis** — the empirical or design rationale for the current value. Acceptable values include "empirical placeholder, see retune trigger" if the value is currently unmeasured; the placeholder string is itself a basis statement and the retune trigger is what discharges it.
2. **Override surface** — where the value can be tuned. The closed enum is:
   - `loomweave.yaml:<field-path>` — operator-tunable via the project config file.
   - `env:<VAR_NAME>` — operator-tunable via environment variable.
   - `plugin.toml:<field-path>` — plugin-author-tunable per ADR-021's plugin-authority split.
   - `recompile` — internal; not exposed.
   - `wire:<spec-anchor>` — pinned on the wire by a wire-contract document; tunable only via an incompatible-version bump in that document.
3. **Retune trigger** — the observable condition that should prompt re-evaluation. The trigger must be expressible as either a metric threshold (e.g., "Pyright RSS exceeds 1.5 GiB before file 25 on a corpus sized at the elspeth scale") or a finding subcode (e.g., "any `LMWV-INFRA-PLUGIN-OOM-KILLED` finding observed in production runs"). "If something feels wrong" does not satisfy the rule.
4. **Coupling** — the constant's relationship to other declared values. The closed enum is:
   - `independent` — standalone; tunable without affecting any sibling constant.
   - `wire-paired-with:<symbol>` — must match a same-shape constant on the other side of a wire contract. A change requires updating both sides in lockstep.
   - `policy-paired-with:<symbol>` — must satisfy a policy invariant against a sibling constant (e.g., a per-session restart cap that must remain less than a per-run restart cap).
   - `floor-of:<symbol>` — a hard floor below which an operator override is refused at config-load time per ADR-021 §2b's "configuration-surface floor" pattern.

The rule's spine is the four-axis declaration; the closed enums on **override surface** and **coupling** keep the rule from drifting into prose. The shape is short — four lines of comment per constant in the steady state.

#### Canonical comment shape

For the Rust workspace, the canonical adjacent-comment shape is:

```rust
/// Operational constant.
///
/// Basis: <one sentence rationale, or "empirical placeholder; see retune trigger">.
/// Override: <one of loomweave.yaml:* | env:* | plugin.toml:* | recompile | wire:*>.
/// Retune: <metric threshold or finding subcode>.
/// Coupling: <one of independent | wire-paired-with:<sym> | policy-paired-with:<sym> | floor-of:<sym>>.
pub const MAX_EXAMPLE_BYTES: usize = 8 * 1024;
```

For the Python plugin, equivalent shape using a module-level `#:` comment block:

```python
#: Operational constant.
#:
#: Basis: <one sentence rationale, or "empirical placeholder; see retune trigger">.
#: Override: <one of loomweave.yaml:* | env:* | plugin.toml:* | recompile | wire:*>.
#: Retune: <metric threshold or finding subcode>.
#: Coupling: <one of independent | wire-paired-with:<sym> | policy-paired-with:<sym> | floor-of:<sym>>.
MAX_EXAMPLE_BYTES = 8 * 1024
```

The lint script (see §5) parses the four named tags exactly as written.

### 2. The eleven operational constants in `loomweave-core`

The 2026-05-22 architecture catalog (`docs/arch-analysis-2026-05-22-1924/02-subsystem-catalog.md` §1 "Concerns") enumerated eleven hardcoded limit constants across `loomweave-core`:

```
MAX_PROTOCOL_ERROR_FIELD_BYTES  (4 KiB,  protocol.rs)
MAX_ENTITY_FIELD_BYTES          (4 KiB,  host.rs)
MAX_ENTITY_EXTRA_BYTES          (64 KiB, host.rs)
STDERR_TAIL_BYTES               (64 KiB, host.rs)
MAX_HEADER_LINE_BYTES           (8 KiB,  transport.rs)
MAX_UNRESOLVED_CALLEE_EXPR_BYTES (512,   host.rs)
ContentLengthCeiling::DEFAULT   (8 MiB,  limits.rs)
EntityCountCap::DEFAULT_MAX     (500_000, limits.rs)
DEFAULT_MAX_RSS_MIB             (limits.rs)
DEFAULT_MAX_NOFILE              (limits.rs)
DEFAULT_MAX_NPROC               (limits.rs)
```

Plus `PYRIGHT_MAX_NPROC = 4096` (host.rs, raised for the language-server runtime). All twelve MUST be retrofitted to the four-axis declaration before the 1.1 release.

For the Python plugin, the inventory enumerated by `answer-python-engineer.md` is:

```
MAX_CONTENT_LENGTH               (8 MiB,  server.py:48,   wire-paired-with ContentLengthCeiling::DEFAULT)
MAX_FILES_PER_PYRIGHT_SESSION    (25,     server.py:49,   operational; see §3 below)
MAX_PYRIGHT_RESTARTS_PER_RUN     (3,      pyright_session.py:44,  policy-paired with the 25-file recycle)
PYRIGHT_INIT_TIMEOUT_SECS        (30.0,   pyright_session.py:46)
PYRIGHT_CALL_TIMEOUT_SECS        (5.0,    pyright_session.py:47)
PYRIGHT_FILE_TIMEOUT_SECS        (3.0,    pyright_session.py:48)
MAX_REFERENCE_SITES_PER_FILE     (2000,   pyright_session.py:45)
MAX_UNRESOLVED_CALLEE_EXPR_BYTES (512,    pyright_session.py:43, wire-paired-with Rust same-name)
STDERR_TAIL_LIMIT                (65536,  pyright_session.py:49, wire-paired-with STDERR_TAIL_BYTES)
```

Plus the writer-actor cadence constants in `crates/loomweave-storage/src/writer.rs`:

```
DEFAULT_BATCH_SIZE       (50,   writer.rs:38)
DEFAULT_CHANNEL_CAPACITY (256,  writer.rs:35)
```

And the crash-loop breaker constants in `crates/loomweave-core/src/plugin/breaker.rs`:

```
CRASH_LOOP_THRESHOLD     (rolling-window count)
CRASH_LOOP_WINDOW        (rolling-window duration)
```

These are the constants the lint script will fail CI on if undeclared after the 1.1 release. Constants discovered after this ADR's authoring inherit the same rule from their first commit — there is no grandfather clause for new code.

### 3. The 25-file Pyright restart constant — instance application

`MAX_FILES_PER_PYRIGHT_SESSION = 25` (`plugins/python/src/loomweave_plugin_python/server.py:49`) is the canonical worked example. Per `answer-python-engineer.md` and `answer-solution-architect.md`, the value was introduced in commit `68b719c` ("Bound Pyright dogfood analysis", 2026-05-20) with no commit-message rationale and no in-file comment. It is an empirical placeholder. The four-axis declaration for it, after retrofit, must be:

- **Basis**: empirical placeholder; bound chosen during dogfood analysis to cap observed Pyright RSS growth across `textDocument/didOpen` cycles; not yet validated against a per-file RSS delta curve.
- **Override**: `plugin.toml:pyright.files_per_session` (plugin-author-tunable per ADR-021's plugin-authority split — the Pyright session recycle is plugin-owned policy, not core-side).
- **Retune**: any sampled Pyright subprocess RSS curve on the elspeth-scale corpus showing the inflection point is below 25 (recycle is wasted re-init cost) or above 25 (`LMWV-INFRA-PLUGIN-OOM-KILLED` risk inside the 25-file window).
- **Coupling**: `policy-paired-with:MAX_PYRIGHT_RESTARTS_PER_RUN` — server-side recycling at 25 files and session-side restart caps interact: the Python engineer documented a concrete failure mode where `_disabled` and `_restart_count` are instance-scoped on `PyrightSession` and reset at every 25-file boundary, breaking the "per run" intent of the restart cap. That interaction is a coupling fact the rule forces to be visible.

The policy-paired-with coupling annotation interlocks with the Python engineer's `answer-python-engineer.md` "Failure mode A": once both constants declare the pairing, the next reviewer asking "is this safe to change in isolation?" reads the coupling and finds the answer in the source.

### 4. File-LOC budget rule

Any source file over **1,500 LOC** declares a split-trigger condition in its module doc-comment. The declaration shape is:

```rust
//! …
//!
//! ## LOC budget
//!
//! Current LOC at last review: <N> (<date>).
//! Split trigger: <one concrete, observable condition>.
//! Rationale for current state: <one sentence>.
```

The 1,500 LOC threshold is the floor. Files **already** over budget at this ADR's authoring receive a one-time grace period: the trigger must be declared in each file's module doc-comment before the 1.1 release. The four files in this category, with the catalog's snapshot LOC alongside the LOC at ADR authoring (the catalog was snapshotted before recent work; the working copy is smaller for two of the four):

| File | Catalog LOC (2026-05-22) | LOC at ADR authoring | Split-trigger (per `answer-solution-architect.md` §2) |
|---|---|---|---|
| `crates/loomweave-mcp/src/lib.rs` | 4,703 | 3,449 | Adding a 20th MCP tool, or any concurrent-tool-execution work. |
| `crates/loomweave-core/src/plugin/host.rs` | 2,935 | 2,935 | Adding a fifth enforcement layer or changing the four-stage pipeline ordering. |
| `crates/loomweave-cli/src/analyze.rs` | 2,549 | 2,427 | A 14th phase added to `run_with_options`'s current 13-phase linearisation. |
| `crates/loomweave-core/src/llm_provider.rs` | 2,467 | 2,467 | See ADR-035 §6 and `answer-solution-architect.md` §3 — this file's split is not "within crate" but "extract to `loomweave-llm`." |

The triggers above are recommendations from `answer-solution-architect.md` §2 and become the declared triggers in each file's module doc-comment via the grace-period retrofit. Each file's owner may revise its declared trigger in a later commit; the rule binds *declaration*, not *which specific trigger is declared*.

### 5. Crate-boundary budget rule

Any crate that takes a dependency widening its trust surface or contradicting its `lib.rs` doc-comment declares the trigger for crate extraction in its top-level `lib.rs` doc-comment. The shape is:

```rust
//! `<crate-name>` — <one-paragraph charter>.
//!
//! ## Crate-boundary budget
//!
//! Boundary statement: <what this crate is for; what is in vs. out>.
//! Extraction trigger: <observable condition that would split content out>.
//! Currently in-scope but extraction-candidate: <list of subsystems with named triggers>.
```

The current cited contradiction (per `answer-solution-architect.md` §3, `answer-security-engineer.md` Q3) is `loomweave-core/src/lib.rs:1`'s doc-comment versus `loomweave-core/src/llm_provider.rs`'s content. The boundary statement says "domain types, identifiers, and provider traits"; the content includes the OpenRouter `reqwest` HTTP transport and two CLI-subprocess providers — neither domain types, nor identifiers, nor trait definitions. **`detailed-design.md:1745` already names `loomweave-llm` as one of the intended workspaces.** That intent + the security argument (an outbound-HTTP-stack CVE inside the plugin-supervisor crate widens the supervisor's trust surface) supplies the extraction trigger: *the `loomweave-llm` extraction MUST land before any new LLM provider is added or before any change to `reqwest` / `rustls` / `hyper` that introduces a new transitive trust dependency.*

`loomweave-core/src/lib.rs` carries this declaration as part of the 1.1 grace-period retrofit. The same rule applies prospectively: a new crate whose `lib.rs` doc-comment is contradicted by its contents must either fix the contradiction or declare an extraction trigger.

### 6. Lint script and CI gate

A lint script lives at `scripts/operational-tuning-lint.{rs,py}` (the implementation language is at the script author's discretion, but the script is run from CI). The script:

1. Walks the Rust workspace under `crates/**/src/**/*.rs` and the Python plugin under `plugins/python/src/**/*.py`.
2. Identifies operational-constant candidates by syntactic match: `pub const` / `const` at module level in Rust; top-level `MAX_*` / `DEFAULT_*` / `*_TIMEOUT_*` / `*_LIMIT_*` / `*_BYTES` / `*_CAP` / `*_THRESHOLD` / `*_WINDOW` identifier patterns in Python. (The pattern set is conservative and pinned in the script; it can be widened in a later commit.)
3. For each candidate, verifies that an adjacent `Basis: … / Override: … / Retune: … / Coupling: …` declaration is present and that `Override` and `Coupling` values come from the closed enums in §1.
4. For each `.rs` file over 1,500 LOC, verifies that the module doc-comment contains an `## LOC budget` section with `Current LOC`, `Split trigger`, `Rationale for current state` lines.
5. For each `lib.rs` whose doc-comment contradicts its content (heuristic: presence of `reqwest` / `tokio::process` / network crates not named in the boundary statement), verifies that a `Crate-boundary budget` section is present.
6. Emits findings in the same JSON shape Loomweave's other tooling uses (subcode prefix `LMWV-DISC-TUNING-*`), one per undeclared constant or undeclared oversize file.

The script is wired into CI as a non-blocking warning until the 1.1 release. **At the 1.1 release the gate flips from warning to failure** — any undeclared operational constant, oversize file, or contradicted crate boundary fails the CI build. The flip date is the trigger for landing the retrofits enumerated in §2, §4, and §5.

The three `#[allow(clippy::too_many_lines)]` sites in `crates/loomweave-cli/src/analyze.rs` (lines 65, 650, 1190) MUST be either re-enabled (the underlying functions split) or replaced with a documented `// allow: ADR-035 §4 — declared split-trigger: <trigger>` comment before the 1.1 release. The clippy threshold itself (`too-many-lines-threshold = 120` per ADR-023's `clippy.toml`) remains the baseline; an `#[allow]` without an ADR-035 reference fails the lint script regardless of whether `cargo clippy` itself passes.

### 7. Constant-graduation lifecycle

A constant's life under the rule has three states:

1. **Hardcoded with declaration.** The constant is in source, has the four-axis declaration, and `Override = recompile`. This is the steady state for constants the operator should not touch — security-uniformity constants per `answer-security-engineer.md` Q5, wire-contract constants whose change is an `api_version` bump, and constants whose retune trigger has not yet fired.
2. **Tunable.** The constant has `Override = loomweave.yaml:<field>` (or `env:<var>` or `plugin.toml:<field>`) and the config-loader actually reads the value. Hard floors enforced at config-load time per ADR-021 §2b are part of this state — the operator can raise the value but not lower it past the floor.
3. **Deprecated.** The constant's basis is replaced by a superseding constant or rule; the declaration carries a `Deprecated: see <new constant or ADR>` line; the constant is removed at the next major version bump.

Graduating a constant from state 1 to state 2 is a normal commit; graduating from state 2 to state 3 requires either an ADR or a documented field-deprecation note in `docs/loomweave/1.0/detailed-design.md`. Demoting from state 2 back to state 1 — removing an operator surface — requires an ADR. This asymmetry codifies the "ratchet" the systems thinker named.

### 8. What is explicitly out of scope for this ADR

- **The wire-pinned batch cap of 256 queries** in `POST /api/v1/files/batch` is pinned by ADR-034 §3 with `Override = wire:contracts.md#batch-cap` semantics. ADR-035's rule applies (the constant must carry a four-axis declaration), but the override surface is not operator-tunable; a change is an `api_version: 2` event by ADR-034's existing rule.
- **`application_id` and `user_version` PRAGMAs** for SQLite are tracked as filigree issue `clarion-f2a984fd6d` per `answer-solution-architect.md` §4 and `answer-quality-engineer.md` Q4. ADR-035's "schema-identity" instance of the rule applies (the PRAGMA value MUST carry a `Basis: identifies the file as Loomweave's per ADR-035` declaration), but the implementation lands as the filigree issue's deliverable, not this ADR's.
- **Specific config-schema design** for `loomweave.yaml` post-1.0 belongs in WP6 per `answer-solution-architect.md` §5. This ADR commits the rule about how constants graduate; the specific YAML key shape is the matter of the work package that implements the graduation.

## Consequences

### Positive

- The five §8 open questions collapse to one durable artifact. Q1 (the 25-file constant), Q4 (PRAGMA identity), and Q5 (eleven hardcoded limits) each gain a recorded basis and a tuning surface; Q2 (four monoliths) and Q3 (`loomweave-llm` extraction) become budgeted properties with named triggers rather than aesthetic preferences competing with sprint load.
- The runtime balancing loops already in the project (path-escape breaker, crash-loop breaker, parent-contains bijection, L2 parity fixture) gain a design-time peer. Per the systems thinker, "the runtime has back-pressure; the architecture itself does not." This ADR is the missing back-pressure at the design-time layer.
- The constant-coupling axis surfaces dangerous interactions that today are invisible. Once `MAX_FILES_PER_PYRIGHT_SESSION` and `MAX_PYRIGHT_RESTARTS_PER_RUN` both carry `policy-paired-with:<sibling>`, the Python engineer's "Failure mode A" (the 25-file boundary silently resetting the per-run restart cap) becomes a fact a reviewer reads in the source, not a defect a future SME has to rediscover. Similarly, the three wire-paired-with Python ↔ Rust constants gain the `# NOTE: must match loomweave-core/…` comment the Python engineer flagged as missing.
- The lint script is the enforcement mechanism that prevents this ADR from being aspirational. A rule the project does not enforce is, per the same drift-to-low-performance archetype, a rule the project does not have. The CI gate is the countervailing signal.
- ADR-021's "configurable" claim becomes a substantive promise on a known timeline. Today, "operator can tune" is aspirational (per `02-subsystem-catalog.md` §1); under this ADR, every constant that says it is tunable has a config-load-time floor and a retune trigger. Constants that say they are not tunable carry that as a deliberate stance (security-uniformity per `answer-security-engineer.md` Q5), not a deferral.
- Cross-product trust-surface arguments become first-class. The `loomweave-llm` extraction (per `answer-security-engineer.md` Q3) is a defense-in-depth win once it lands: an outbound-HTTP-stack CVE no longer reaches the plugin-supervisor crate. The crate-boundary budget rule (§5) gives that extraction a declared trigger rather than a vague intention in `detailed-design.md`.

### Negative

- Source comment volume increases. Every operational constant gains 4–5 lines of comment. For the twelve constants in `loomweave-core` plus the nine in the Python plugin plus the two writer-actor cadence constants plus the two breaker constants, the retrofit is on the order of 100 lines of comment across the workspace — bounded but non-trivial.
- The lint script is one more piece of in-repo tooling to maintain. ADR-023's existing five CI gates become six (fmt, clippy, nextest, doc, deny, **tuning-lint**). The script must keep pace with new constant-naming patterns (e.g., a future `MAX_CALLEE_DEPTH` that doesn't match the current `MAX_*_BYTES` heuristic). Mitigation: the script's pattern set is in source, reviewable, and one PR away from a fix.
- The four-axis declaration imposes authoring friction on new constants. A contributor adding a new `MAX_FOO` must, before merging, identify the constant's basis, decide its override surface, name its retune trigger, and trace its coupling. This is the intended shape — the friction is the rule — but it raises the per-commit cost of adding limits compared to today.
- The 1,500-LOC budget threshold is a judgement call. Set lower, it would force more files to declare triggers (potentially valuable but noisier); set higher, it would let `host.rs` and `analyze.rs` evade declaration. 1,500 LOC is chosen as the floor where a file becomes hard to read end-to-end in one sitting; it is not derived from a study. Mitigation: this threshold is itself an operational constant under this ADR's rule, and its retune trigger is "any file declares a trigger and the trigger is consistently 'never'" — a sign the threshold is too lax.
- The `loomweave-llm` extraction trigger commits a future split that may bring its own churn. Per `answer-solution-architect.md` §3, the LLM surface is at its minimum scope right now (post-ADR-030 narrowing of WP6); the split is cheap today and expensive later. The risk is that "ship the v1.0 tag" pressure interacts with "land the crate extraction" and either delays the tag or rushes the split. Mitigation: the trigger names the condition (any new LLM provider, any change to the network-stack transitive deps), so the split is reactive to a forcing function rather than scheduled.

### Neutral

- This is a level-5 (rules) Meadows intervention. Leverage-point hierarchy: level 12 is "parameters" (the constants themselves), level 6 is "information flows" (the four-axis declaration is one), level 5 is "rules" (this ADR). A level-12 intervention — just adding `loomweave.yaml` keys without the rule — would, per `answer-systems-thinker.md`, decay back to hardcoded constants by the next sprint. A level-6 intervention — emitting metrics on every constant's observable behaviour — would over-instrument before knowing which constants matter. The level-5 rule is the floor: it forces declaration without forcing exposure or instrumentation; the exposure and instrumentation become subsequent commits gated by declared retune triggers.
- The "internal, no override" state (state 1 in §7) is an allowed terminal state. A constant whose basis is "uniform security policy across deployments per ADR-035 §3 stance" and whose override surface is `recompile` is fully compliant with the rule. The discipline is *declaration*, not *exposure*; the security engineer's concern that exposing all 11 limits as operator surface creates a support burden (`answer-security-engineer.md` Q5) is preserved by allowing this state.
- ADR-029's entity-association binding is a different mechanism for a different problem. ADR-029 binds Filigree issues to source entities; ADR-035 imposes a source-comment discipline on operational constants. The two mechanisms can be combined (an unresolved retune trigger could be filed as a filigree issue and bound to the constant's entity ID via ADR-029) but neither requires the other. This ADR is not a Filigree integration.
- ADR-034 §3's wire-pinned batch cap of 256 queries is an instance of the rule, not an exception to it. The constant carries `Override = wire:contracts.md#batch-cap` and `Coupling = wire-paired-with:Filigree client splitting logic`. The override-surface enum's `wire:*` value exists exactly for this case.
- This ADR's "lint script in CI" enforcement mechanism is itself an operational constant under the rule (its `Retune` trigger is "any retroactive retrofit of new constants becomes routinely required at code-review time rather than at lint-script time, suggesting the lint set is too narrow"). The rule applies to itself.

## Alternatives Considered

### Alternative 1: ship `loomweave.yaml` with all eleven constants as keys and call it done

**Pros**: most operationally tactile; an operator can `cat loomweave.yaml` and see what is tunable. Implementation surface is bounded — eleven keys, eleven config-loader sites, eleven floor checks. Matches ADR-021 §4's existing four named keys and extends them naturally to the other seven.

**Cons**: this is a level-12 (parameters) intervention to a drift-to-low-performance loop. Per `answer-systems-thinker.md`:

> "A config file without the rule decays back to hardcoded constants on the next sprint — that is exactly how Loomweave got here. The rule is what creates the surface; the surface alone is a parameter intervention (Level 12) and parameter interventions to a drift-to-low-performance loop reset the constant without changing the slope."

The eleven constants in `loomweave-core` were not added all at once; they accreted over Sprints 1 and 2. Adding `loomweave.yaml` without the rule that gates how the *next* constant graduates means the twelfth, thirteenth, fourteenth constants land hardcoded again, with `breaker.rs:7`-style "lands in WP6" comments, and the cycle repeats. The discipline must precede the surface, not follow it.

**Why rejected**: the parameter intervention does not change the rate at which new constants accrete; only the rule does.

### Alternative 2: defer the rule to the WP6 work package post-1.0

**Pros**: matches the solution architect's "ship 1.0 with the limits hardcoded; land the config surface in WP6 post-1.0 as one ADR-021-aligned change" recommendation almost verbatim. Avoids ADR churn during release cut. The five §8 questions are not 1.0 blockers; the gap-register has `application_id`/`user_version` as the only constant-related v1.0 blocker, and that has its own filigree issue.

**Cons**: the solution architect's recommendation is about *implementation timing* of the config surface, not about *whether the rule exists*. The rule and the implementation are separable: this ADR commits the rule now (level 5), and WP6's post-1.0 implementation lands the surface on the rule's existing framework (level 12 underneath the level-5 rule). Deferring the rule itself means WP6 lands the surface without a governing discipline — the same configuration-without-rule trap Alternative 1 falls into, with a one-WP delay.

The systems thinker's recommendation is explicit: *"Promote it to Accepted before any further hardcoded limit lands."* The cost of accepting the rule now is the four-axis-declaration retrofit. The cost of accepting the rule later is that any constant added between now and WP6 lands without the discipline, has to be retrofitted twice (once for the rule, once for the WP6 graduation), and the lint-script gate flips later, lengthening the drift window.

**Why rejected**: rule and implementation are separable; accepting the rule now is cheaper than accepting it later, and the rule is what makes the implementation durable.

### Alternative 3: rely on code review to catch undeclared constants

**Pros**: no new tooling. Reviewers already inspect new constants for naming, type, and call-site usage; adding "is there a retune trigger?" to the review checklist is one more line on a checklist.

**Cons**: code review is a probabilistic catch, not a deterministic one. The systems thinker's archetype — drift to low performance — predicts exactly the failure mode where reviewers locally accept "we'll document this later" and the documentation never lands. The five §8 questions are evidence the catch-rate is below what the project needs. The literal `#[allow(clippy::too_many_lines)]` at `analyze.rs:65` is what reviewer-only enforcement looks like in this codebase today: a reviewer accepted the allow because it shipped the function; the standard-lowering act is visible in code.

A CI lint script is deterministic: the build either fails or it does not. The countervailing signal is mechanical, not human, and runs on every PR.

**Why rejected**: code review's catch-rate is what this ADR is trying to compensate for, not augment.

### Alternative 4: extract `loomweave-llm` immediately as the high-impact intervention; defer the broader rule

**Pros**: closes the security-engineer's STRIDE-T/STRIDE-I argument fastest (the outbound-HTTP stack stops sharing a crate with the plugin supervisor); concrete deliverable; survives `cargo clippy`.

**Cons**: solves one of the five questions and leaves the other four unaddressed. The five questions are one pattern; intervening on Q3 alone is a parameter-level move on a single constant (the crate boundary) without changing the slope on the others. The next sprint adds the twelfth hardcoded limit and the §8 list grows from five questions to six.

Also: the extraction *is* part of this ADR (§5's crate-boundary budget rule, with the trigger naming exactly the security-engineer's condition). Accepting the rule causes the extraction; accepting only the extraction does not cause the rule.

**Why rejected**: a single-constant intervention does not address a pattern of multiple constants accreting under the same archetype.

### Alternative 5: an ADR that demands a richer declaration (per-constant doc page; per-constant test; per-constant metric)

**Pros**: maximum traceability; a future architect could query any constant and find its full lineage.

**Cons**: scope-creep into ceremony. The systems thinker's risk register named this explicitly:

> "An ADR that demands a basis for every constant could ossify into ceremony. Mitigation: the basis statement is one sentence; the trigger is one finding subcode. Anything more is the wrong shape."

A per-constant test is the right discipline for *some* constants (the security-enforcement cluster per `answer-quality-engineer.md` Q5) but not all (the ring-buffer-overflow eviction in `STDERR_TAIL_BYTES` is "Partially tested" and acceptable). A per-constant metric over-instruments before knowing which constants matter operationally. A per-constant doc page is what `02-subsystem-catalog.md` already produces at architecture-review time; pre-producing them for every constant inverts the relationship.

The four-axis declaration is the floor — additional discipline can be layered above it (a quality-engineering follow-up to add a behavioural test for `DEFAULT_MAX_RSS_MIB` is fully within scope of the current quality engineering sheets) without enlarging the rule itself.

**Why rejected**: the rule's shape — short, mechanical, lint-checkable — is what makes it a level-5 intervention rather than ceremony. Richer shapes are level-6/7 interventions appropriate for specific constant clusters, not the workspace-wide floor.

## Related Decisions

- [ADR-021](./ADR-021-plugin-authority-hybrid.md) — Plugin authority hybrid; §4 names four of the eleven operational constants (`plugin_limits.max_frame_bytes`, `plugin_limits.max_records_per_run`, `plugin_limits.max_rss_mib`, `expected_max_rss_mb`) as `loomweave.yaml` config keys. ADR-035 generalises the discipline that ADR-021 §4 already implies and extends it to every operational constant.
- [ADR-023](./ADR-023-tooling-baseline.md) — Tooling baseline; defines `clippy.toml`'s `too-many-lines-threshold = 120`. ADR-035 §4's 1,500-LOC budget operates above ADR-023's per-function threshold — the two thresholds apply to different units (function vs. file) and do not conflict. The `#[allow(clippy::too_many_lines)]` sites in `analyze.rs` are ADR-023 escapes; ADR-035 §6 requires either re-enabling them or pairing each with a declared split trigger.
- [ADR-030](./ADR-030-on-demand-summary-scope.md) — Narrowed WP6 to the on-demand `summary(id)` MCP tool, leaving the operator-tunables work currently un-homed per `answer-solution-architect.md` §5. ADR-035 commits the governing rule; the implementation of the config surface lands in a post-1.0 work package (TBD) that operates on top of this ADR's framework.
- [ADR-034](./ADR-034-federation-http-read-api-hardening.md) — Federation HTTP read API hardening; §3's wire-pinned 256-query batch cap is an instance of ADR-035's rule (`Override = wire:contracts.md#batch-cap`). The two ADRs do not conflict; ADR-034 specifies the wire contract, ADR-035 specifies the source-comment discipline.
- [ADR-029](./ADR-029-entity-associations-binding.md) — Entity-association binding; explicitly orthogonal to ADR-035. An unresolved retune trigger could be filed as a filigree issue and bound to the constant's entity ID via ADR-029, but the binding is not required.

## References

### Originating analysis (2026-05-22 architecture review)

- Final report `§8 Open Questions`: [`docs/arch-analysis-2026-05-22-1924/04-final-report.md`](../../arch-analysis-2026-05-22-1924/04-final-report.md#8-open-questions-for-the-next-phase) — the five questions this ADR's rule absorbs.
- Subsystem catalog `loomweave-core` Concerns: [`02-subsystem-catalog.md`](../../arch-analysis-2026-05-22-1924/02-subsystem-catalog.md) §1 — the eleven-limit enumeration; the "every limit is a recompile" tell; the `mock.rs` 876-LOC sign of non-trivial host pipeline state.
- Subsystem catalog `loomweave-storage` Concerns: §2 — the `application_id`/`user_version` gap; the writer-actor cadence constants (256 / 50) at `writer.rs:35,38`.

### SME roundtable (2026-05-23)

- Solution architect: [`answer-solution-architect.md`](../../archive/working-notes/arch-analysis-2026-05-22-1924/answer-solution-architect.md) — WP6 home triangulation; the "ship 1.0 with limits hardcoded; land config surface in WP6 as one ADR-021-aligned change" frame; the per-file split-trigger table.
- Systems thinker: [`answer-systems-thinker.md`](../../archive/working-notes/arch-analysis-2026-05-22-1924/answer-systems-thinker.md) — the level-5 (rules) leverage argument; the drift-to-low-performance archetype; the `analyze.rs:74` and `breaker.rs:7` smoking-gun tells (line numbers as recorded at analysis time; current `analyze.rs` `#[allow(clippy::too_many_lines)]` site has shifted to line 65 with two additional sites at 650 and 1190; the rule applies to all three).
- Python engineer: [`answer-python-engineer.md`](../../archive/working-notes/arch-analysis-2026-05-22-1924/answer-python-engineer.md) — the wire-contract-pinned vs. operational-tunable Python constant classification; the `MAX_PYRIGHT_RESTARTS_PER_RUN` "per run" name vs. per-instance implementation interaction failure; the basis for the `Coupling` axis.
- Quality engineer: [`answer-quality-engineer.md`](../../archive/working-notes/arch-analysis-2026-05-22-1924/answer-quality-engineer.md) — the per-constant test-coverage matrix; the `DEFAULT_MAX_RSS_MIB`/`DEFAULT_MAX_NOFILE`/`DEFAULT_MAX_NPROC` security-enforcement cluster as the highest-risk untested area.
- Security engineer: [`answer-security-engineer.md`](../../archive/working-notes/arch-analysis-2026-05-22-1924/answer-security-engineer.md) — the STRIDE-D/STRIDE-E framing of recompile-to-tune as a security-posture stance; the `loomweave-llm` extraction as STRIDE-T/STRIDE-I defense-in-depth; the security-uniformity argument for keeping some constants `Override = recompile`.

### Source-of-truth code locations

- `crates/loomweave-core/src/plugin/limits.rs` — `ContentLengthCeiling::DEFAULT`, `EntityCountCap::DEFAULT_MAX`, `DEFAULT_MAX_RSS_MIB`, `DEFAULT_MAX_NOFILE`, `DEFAULT_MAX_NPROC`.
- `crates/loomweave-core/src/plugin/host.rs` — `MAX_ENTITY_FIELD_BYTES`, `MAX_ENTITY_EXTRA_BYTES`, `STDERR_TAIL_BYTES`, `MAX_UNRESOLVED_CALLEE_EXPR_BYTES`, `PYRIGHT_MAX_NPROC`.
- `crates/loomweave-core/src/plugin/breaker.rs:7` — the literal "lands in WP6" comment that this ADR retires; `CRASH_LOOP_THRESHOLD`, `CRASH_LOOP_WINDOW`.
- `crates/loomweave-core/src/plugin/protocol.rs` — `MAX_PROTOCOL_ERROR_FIELD_BYTES`.
- `crates/loomweave-core/src/plugin/transport.rs` — `MAX_HEADER_LINE_BYTES`.
- `crates/loomweave-storage/src/writer.rs:35,38` — `DEFAULT_CHANNEL_CAPACITY = 256`, `DEFAULT_BATCH_SIZE = 50`.
- `crates/loomweave-cli/src/analyze.rs:65,650,1190` — three `#[allow(clippy::too_many_lines)]` sites that ADR-035 §6 requires either re-enabling or pairing with a declared split trigger before the 1.1 release.
- `plugins/python/src/loomweave_plugin_python/server.py:48,49` — `MAX_CONTENT_LENGTH = 8 MiB` (wire-paired-with Rust), `MAX_FILES_PER_PYRIGHT_SESSION = 25`.
- `plugins/python/src/loomweave_plugin_python/pyright_session.py:43-49` — six Pyright-session operational constants enumerated by `answer-python-engineer.md`.

### Doctrine the rule operationalises

- Meadows, Donella H., *Thinking in Systems: A Primer*. Level-5 (rules) and level-6 (information flows) leverage points in the twelve-level hierarchy.
- The doctrine in [`docs/suite/weft.md`](../../suite/weft.md) §5 — the federation axiom's "enrich-only" rule applies by analogy to constants: a constant that gates externally observable behaviour without a declared basis is the same archetype as a sibling tool that adds a required dependency.

— End of ADR-035 —
