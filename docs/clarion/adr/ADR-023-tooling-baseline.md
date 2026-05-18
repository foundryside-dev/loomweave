# ADR-023: Rust + Python Tooling Baseline at the Zero-Code Frontier

**Status**: Accepted
**Date**: 2026-04-18
**Deciders**: qacona@gmail.com
**Context**: first implementation commits (Sprint 1 WP1) are about to land in a
documentation-only repository. The workspace's lint, format, edition, test-runner,
supply-chain, CI, and type-check posture is about to be locked into the first
commit graph — either deliberately or by default. Setting these surfaces now
costs close to zero; retrofitting after any Clarion/Wardline-scale code has
been written is expensive.

## Summary

The Clarion workspace adopts a strict tooling baseline from its first code
commit, before any implementation lands:

- **Rust**: edition **2024**, workspace-level `[lints]` block with
  `clippy::pedantic = "warn"` + `unsafe_code = "forbid"`, `rustfmt.toml`
  pinned, `clippy.toml` with relaxed pedantic thresholds, `cargo-nextest` as
  the test runner, `cargo-deny` for supply-chain hygiene, GitHub Actions CI
  running fmt-check, pedantic clippy, nextest, deny, and doc build.
- **Python** (plugin side): `ruff` for lint + format (strict config),
  `mypy --strict` from the first commit, `pytest` for tests, `pre-commit`
  wiring ruff + mypy into every `git commit`.
- **ADR precedent**: this baseline is the floor; later WPs may raise it
  (coverage gating, cargo-audit, nightly-only rustfmt options) but may not
  lower it without a superseding ADR.

Sprint 1 Work Package 1 (scaffold + storage) is the implementation trigger.
WP1 Task 1 ships all the configuration files named below. WP3 Task 1 ships
the Python equivalents.

## Context

The Clarion repository sat at zero lines of code on 2026-04-18. Sprint 1's
WP1 was about to land a three-crate Cargo workspace, a SQLite migration, a
writer-actor, and a CLI skeleton. The original WP1 plan (UQ-WP1-09 resolution
at `docs/implementation/sprint-1/wp1-scaffold.md`) committed to Rust edition
2021 + the default `cargo test` runner + no formal lint gate beyond
`-D warnings`.

That resolution was written under "fine to document and move on" framing —
the canonical tell for decisions that survive into production as unexamined
baselines. Three observations forced the re-examination:

1. **Edition 2024 has been stable since February 2025.** No v0.1 dependency
   in the planned graph (`rusqlite`, `deadpool-sqlite`, `tokio`, `clap`,
   `thiserror`, `tracing`) constrains to 2021. The 2021 choice was
   inherited, not motivated.
2. **Workspace-level `[lints]` with pedantic enabled is near-free at
   greenfield and expensive to retrofit.** Each crate that exists before
   pedantic is introduced must be audited and silenced or fixed; starting
   pedantic-clean means every new contribution passes against the strict
   floor from day one. The scope-commitment memo already commits Clarion to
   "enterprise rigor at lack of scale" (`plans/v0.1-scope-commitments.md`).
   Pedantic is the cheapest expression of that commitment that exists.
3. **Sprint 1 has four `cargo test` call sites today**; Sprint 2+ will have
   dozens. Moving to `cargo-nextest` after the first sprint forces a
   workspace-wide find-and-replace across CI, docs, and runbooks. Moving
   now is a one-line `cargo.toml` change plus a single `install-action`
   step in CI.

The Python side runs the same argument. UQ-WP3-10 in `wp3-python-plugin.md`
deferred mypy "until the plugin grows enough to benefit." That's the same
"fine to document and move on" frame: every Python module written without
mypy-strict accumulates as a retrofit surface. Adopting mypy-strict at
Task 1 means every extractor, probe, and server-loop module is written
with full type coverage from the first keystroke.

## Decision

### Rust (workspace-wide)

**Edition**: 2024.

**Toolchain pin** (`rust-toolchain.toml` at repo root):

```toml
[toolchain]
channel = "stable"
components = ["clippy", "rustfmt", "llvm-tools-preview"]
profile = "minimal"
```

`llvm-tools-preview` is included because it costs nothing at install time
and makes `cargo install cargo-llvm-cov` work first try if a later WP wants
local coverage — the retrofit path cost would be higher than carrying the
component from the start.

**Workspace `[lints]` block** (in root `Cargo.toml`):

```toml
[workspace.lints.rust]
unsafe_code = "forbid"

[workspace.lints.clippy]
pedantic = "warn"
# Pragmatic allows — revisit per WP if the floor is too loud:
module_name_repetitions = "allow"
must_use_candidate = "allow"
missing_errors_doc = "allow"
```

Every member crate declares `lints.workspace = true` so a later-added crate
cannot drift off the baseline.

**`rustfmt.toml`**:

```toml
edition = "2024"
max_width = 100
newline_style = "Unix"
use_field_init_shorthand = true
use_try_shorthand = true
```

**`clippy.toml`**:

```toml
cognitive-complexity-threshold = 15
too-many-arguments-threshold = 8
too-many-lines-threshold = 120
```

**Test runner**: `cargo nextest run` (a dev-dep install managed via
`taiki-e/install-action@cargo-nextest` in CI). Exit criteria and demo
scripts use `cargo nextest run`, not `cargo test`.

**Supply-chain**: `cargo-deny` with `deny.toml` (v2 schema) checking
advisories (`yanked = "deny"`), license allowlist (MIT, Apache-2.0,
Apache-2.0 WITH LLVM-exception, BSD-2-Clause, BSD-3-Clause, ISC,
Unicode-3.0, Unicode-DFS-2016), multi-version `"warn"`, wildcards `"deny"`,
unknown-registry/unknown-git `"deny"`.

**CI** (`.github/workflows/ci.yml` on push to main + every PR) runs:

1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo nextest run --all-features`
4. `cargo doc --no-deps --all-features`
5. `cargo deny check`

Any PR merging to main must pass all five gates.

### Python (plugin side)

**Tooling stack**:

- **`ruff`** — lint + format. Strict config at `plugins/python/ruff.toml` or
  `[tool.ruff]` in `pyproject.toml`. Select rules: `ALL` minus pragmatic
  excludes (`D` docstring lints relaxed; `COM812` / `ISC001` that conflict
  with format; explicit per-file-ignores for tests and fixtures).
- **`mypy --strict`** from day 1. Config at `plugins/python/mypy.ini` or
  `[tool.mypy]` block; `strict = true` plus explicit module entries for
  third-party deps without stubs.
- **`pytest`** + `pytest-cov`. Coverage reported but not gated in Sprint 1;
  a WP6-era coverage floor may be added later as a raise-the-ceiling change.
- **`pre-commit`** with hooks for `ruff check`, `ruff format`, `mypy`.
  Installed via `pre-commit install` after `pip install -e .[dev]`.

**CI extension** (same workflow, separate job): install `uv`, install the
plugin editable with dev extras, run `ruff check`, `ruff format --check`,
`mypy --strict`, `pytest`.

### ADR precedent

- This baseline is a **floor**. Future WPs may tighten (e.g., add a
  coverage-% gate, promote `missing_errors_doc` to warn) but must not
  loosen. Loosening requires a superseding ADR with a named justification.
- Tool version pins live in CI (`taiki-e/install-action` resolves latest by
  default; pin to exact version if a WP hits a regression). Workspace
  `Cargo.toml` never downgrades edition; `rust-toolchain.toml` never
  regresses its channel.

## Alternatives Considered

### Alternative 1: retain the original WP1 baseline (edition 2021, `cargo test`, no CI)

**Pros**: matches the pre-authored WP1 Task ledger verbatim; zero doc churn;
faster to start writing code in the current session.

**Cons**: bakes in three retrofit surfaces (edition migration, pedantic
introduction, CI wiring) that compound with every commit that lands before
they're addressed. The WP1 author (same author as this ADR, several hours
earlier) flagged UQ-WP1-09 as "fine to document and move on" — the canonical
signal for decisions that deserve re-examination precisely because nobody
has looked at them twice.

**Why rejected**: the cost of changing direction at commit zero is the cost
of re-running this doc edit plus Task 1 reprep. The cost of changing
direction at commit 500 is auditing every line of code against a new lint
floor. The asymmetry is large enough that "the plan already says so" is not
a sufficient reason to hold.

### Alternative 2: adopt the baseline but defer Python's mypy-strict

**Pros**: WP3 starts faster; Python's first iterations are less type-churn.

**Cons**: mypy-strict retrofit is identical in shape to pedantic retrofit —
every module written without it is a module to audit. The plugin's first
module (Task 1 Python package skeleton) is ~20 lines; writing it against
strict is a trivial fraction of the authoring time.

**Why rejected**: the same cost-asymmetry argument that rejects Alternative
1 also rejects this partial version.

### Alternative 3: adopt everything plus coverage % gating + cargo-audit + nightly rustfmt

**Pros**: maximum strictness posture.

**Cons**: coverage % needs real code density before a meaningful floor
emerges (Sprint 1 is too small to set one without hand-tuning). `cargo-deny`
advisories database subsumes `cargo-audit` — running both is duplicate
work. Nightly rustfmt options (`imports_granularity`, `group_imports`)
require every contributor to install a nightly toolchain or CI to pin one.

**Why rejected**: scope-creep past "floor at the zero-code frontier" into
"everything a mature project eventually has." The baseline names exactly
the surfaces that are cheap now and expensive later; the three above are
either scale-dependent (coverage) or redundant (cargo-audit) or a
cross-team burden (nightly rustfmt).

### Alternative 4: defer CI to "when the repo becomes shared"

**Pros**: saves the ~20-line GitHub Actions file and one-time CI-wiring
debugging.

**Cons**: CI for a solo-maintainer repo is not about collaboration — it's
about discipline. Running the five-gate sequence on every PR ensures no
local-only `cargo check` can ever pass into main. The cost of writing a
fresh CI workflow from memory at Sprint 5 is higher than the cost of
starting one at Sprint 1 and letting it accumulate minor additions.

**Why rejected**: CI's value is insurance against the class of errors that
manifest only when something runs on a clean machine. That class exists
from commit one. The workflow is small enough that "defer until needed"
generates less value than the discipline floor it provides.

## Consequences

### Positive

- Every future Clarion commit passes pedantic clippy, rustfmt, cargo-deny,
  and — for Python — ruff + mypy-strict. The debt load is structurally
  bounded at zero.
- New contributors (or new-me after a context switch) inherit the same
  floor without needing to re-litigate it. `clippy.toml` +
  `rustfmt.toml` + `deny.toml` + `mypy.ini` are self-documenting.
- Edition 2024 gives first-class access to 2024-era features (improved
  `let-else` diagnostics, RPITIT stabilisations, etc.) without a migration
  gate later.
- `cargo nextest` halves test-suite wall-clock time on workspace builds
  versus `cargo test`; for WP1's already-growing test count (12 integration
  tests in `clarion-storage` alone), the compounded savings are non-trivial.
- ADR-023's existence as a discoverable decision record means "why is this
  pedantic?" answers itself without a git-archaeology trip.

### Negative

- Every lint expansion or strictness increase (`clippy::pedantic` ships
  with ~50 lints; a Rust-stable release may add more) can, in principle,
  break a future `cargo clippy` run after a toolchain update. Mitigation:
  `rust-toolchain.toml` pins the channel, so upgrades are explicit events;
  CI catches breakage at PR time, not at master.
- `mypy --strict` imposes real authoring cost on Python code — every `dict`
  needs its key/value types spelled out, every `None` return needs its
  annotation, every callable argument needs its signature. For the Sprint
  1 Python plugin scope (one extractor, one probe, one JSON-RPC loop,
  ~300 LOC) this is acceptable.
- Pedantic's pragmatic allows (`module_name_repetitions`,
  `must_use_candidate`, `missing_errors_doc`) are judgment calls. A later
  WP that finds one of them burying a real bug should author a
  superseding-lint ADR to tighten the allow-list.
- GitHub Actions CI introduces a dependency on GitHub's CI infrastructure
  for the merge gate. An extended GitHub outage can't be worked around by
  pushing straight to main without the five gates passing. Mitigation:
  local pre-commit (Python) + `./scripts/ci-local.sh` (Rust, not yet
  written — WP1 or Sprint 2 can add) lets solo contributors run the same
  sequence offline.

### Neutral

- `cargo-deny`'s license allowlist is not exhaustive; new dependencies
  with unlisted licenses will fail `cargo deny check` and require an
  explicit allowlist expansion (with a commit message noting the license
  and reason). This is the intended shape — surfacing license decisions,
  not burying them.
- `rust-toolchain.toml` pinning to `channel = "stable"` means local
  `cargo` installs float with whatever stable rustc is current when a
  contributor runs `rustup update`. If Sprint 5 wants reproducible
  toolchain versions, pin to a specific `1.XY.Z` string in a single
  commit.

## Related Decisions

- [ADR-001](./ADR-001-rust-for-core.md) — picks Rust as the core
  implementation language. ADR-023 is ADR-001's operational complement:
  given Rust, here is how we write it.
- [ADR-011](./ADR-011-writer-actor-concurrency.md) — locks the
  `rusqlite` + `deadpool-sqlite` + `tokio` crate stack. ADR-023 pins the
  edition those crates compile against and the lint floor every call site
  to them must pass.
- [ADR-022](./ADR-022-core-plugin-ontology.md) — sets the manifest-acceptance
  contract. Plugins authored in Python (WP3) or later in other languages
  can adopt tooling baselines appropriate to their ecosystem; the Rust
  host's baseline is defined here.
- Scope-commitment memo [`../../implementation/v0.1-scope-plans/v0.1-scope-commitments.md`](../../implementation/v0.1-scope-plans/v0.1-scope-commitments.md)
  — "enterprise rigor at lack of scale" is the phrase this ADR operationalises
  at the tooling layer.

## References

- [Sprint 1 WP1 scaffold plan](../../implementation/sprint-1/wp1-scaffold.md) —
  UQ-WP1-09 (Rust toolchain) is revised to reference this ADR.
- [Sprint 1 WP3 Python plugin plan](../../implementation/sprint-1/wp3-python-plugin.md) —
  UQ-WP3-10 (Python tooling) is revised to reference this ADR.
- [Rust 2024 edition guide](https://doc.rust-lang.org/edition-guide/rust-2024/index.html) —
  stabilised features and migration notes.
- [cargo-deny v2 schema](https://embarkstudios.github.io/cargo-deny/checks/cfg.html) —
  the config syntax `deny.toml` uses.
- [Clippy pedantic lint group](https://rust-lang.github.io/rust-clippy/stable/index.html#/level=pedantic) —
  the lint set this ADR adopts at `warn`.
