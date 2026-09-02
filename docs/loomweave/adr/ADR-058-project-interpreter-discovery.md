# ADR-058: Project Interpreter Discovery for Pyright

**Status**: Accepted — amended 2026-09-02 (see Amendment below)
**Date**: 2026-08-29
**Deciders**: john
**Related**: [ADR-021](./ADR-021-plugin-authority-hybrid.md), [ADR-035](./ADR-035-operational-tuning-discipline.md), [ADR-050](./ADR-050-plugin-lifecycle-deadlines.md), [ADR-057](./ADR-057-pyright-restart-attribution.md)
**Tickets**: clarion-5cf9643de9

## Context

`pyright-langserver` decides which Python environment to type-check against by
running whatever `python` is first on its `PATH`, unless the client sets
`python.pythonPath`. Under `loomweave analyze` launched from an agent hook,
`python` on `PATH` is the system interpreter, which cannot import the target
project's editable install — so every `tests/` → `src/` call target came back
empty while the per-facet coverage claim still said `complete`.

The bisect that pinned the cause, same file and same build of the Python
plugin, three launcher environments:

| Launcher environment | Total calls resolved | Calls into `elspeth.*` | Coverage claim |
|---|---|---|---|
| venv python on `PATH` | 39 | 13 | `complete` |
| `VIRTUAL_ENV` set, venv **not** on `PATH` | 26 | 0 | `complete` |
| clean `env -i` (nothing set) | 26 | 0 | `complete` |

All three runs claimed `complete`. The difference was invisible on the
coverage surface, and the incremental content-hash skip then pinned the
launcher-dependent hole permanently: 400 Python test files on elspeth were
locked to whichever answer their first analyze happened to get, regardless of
which environment a later run was launched from.

## Decision

### 1. A fixed, cross-language discovery order

Both the host (`crates/loomweave-core/src/plugin/interpreter.rs`) and the
Python plugin (`plugins/python/src/loomweave_plugin_python/interpreter.py`)
run the identical discovery order — a cross-language contract the two source
files each say must not be changed on one side alone:

| Rung | Source | `ProjectInterpreter.source` | Pinned? |
|---|---|---|---|
| 1 | `LOOMWEAVE_PYTHON_INTERPRETER` env var names an executable file | `override` | yes |
| 2 | `<project_root>/.venv/bin/python` — only when not repository-tracked (Amendment 2026-09-02) | `dotvenv` | yes |
| 3 | `$VIRTUAL_ENV/bin/python` | `virtual_env` | yes |
| 4 | `$CONDA_PREFIX/bin/python` | `conda` | yes |
| 5 | first `python` / `python3` on `PATH` | `path` | no |
| 6 | nothing found | `none` | no |

An empty env value counts as unset at every rung (not just a missing key),
but the two sides reach that by different routes. Python filters explicitly on
the override rung (`if override:`) and on `VIRTUAL_ENV`/`CONDA_PREFIX`
(`if prefix and ...`); on the `PATH` rung it relies on `shutil.which(name,
path="")` returning `None` (CPython >= 3.8), so no explicit filter is written
there. Rust filters explicitly on all four, because it must: an unfiltered
empty `PATH` degrades to a CWD-relative `python` lookup — `split_paths("")`
yields one empty entry, and `"".join("python")` is stat'd against whatever
directory the `analyze` process happened to be started in.

The returned path is **absolute and lexically normalised, never
symlink-resolved**: Python does `Path.absolute()` + `os.path.normpath`; Rust
does `std::path::absolute` + a matching lexical `..`-collapse. A venv's
`bin/python` is typically a symlink to the base interpreter; handing pyright
the symlink path keeps it inside the project's venv site-packages, while
resolving to the realpath would escape to the base interpreter's
site-packages. Executability is judged by `access(2)` with `X_OK` (Rust via
`nix::unistd::access`, Python via `os.access(..., os.X_OK)`) — real uid/gid,
ACLs, and `noexec` mounts included — never raw mode bits, so the two sides
agree on which candidates are usable.

### 2. `python.pythonPath` in the LSP configuration reply

When `PyrightSession` answers pyright's `workspace/configuration` request for
the `python` section, it replies `{"pythonPath": <interpreter.path>,
"analysis": {...}}` when discovery found a path, or just `{"analysis":
{...}}` when it found none — letting pyright fall back to its own default
(`_configuration_for_section` in `pyright_session.py`). The interpreter used
is logged once per pyright subprocess, not once per query
(`_announce_interpreter_once`, guarded by `_interpreter_announced`), naming
the path, `source`, and whether it is `pinned`.

### 3. `interpreter_unpinned` coverage semantics

When discovery lands on `path` or `none` (unpinned), `_environment_qualified`
demotes an otherwise-`complete` calls/references facet to `degraded` with
reason `interpreter_unpinned`, `transient=false`, `collateral=false`
(`pyright_session.py`). Three rulings, all load-bearing:

- **Only demotes `complete`.** A facet that is already `degraded` for another
  reason (a real pyright timeout, a syntax error) keeps its own reason —
  the interpreter is not the cause of that hole and must never mask it
  (ADR-057's attribution discipline: the catch-site decides, never message
  text).
- **`transient=false`, not `true`.** `interpreter_unpinned` is
  environment-determined, exactly like `syntax_error` / `reference_site_cap`,
  and joins ADR-057 §1's list of tokens "outside the attribution question."
  It is deliberately excluded from the re-dispatch budget those tokens spend:
  see Alternatives below for why `transient=true` was rejected.
- **`collateral=false`.** The file itself did nothing wrong; the run's
  environment did. It is not swept into another file's poison window the way
  a collateral pyright death is (ADR-057 §4's sticky mark).

### 4. `plugin_index_meta.resolver_environment` and the fingerprint

Migration `0015_plugin_resolver_environment.sql` adds
`plugin_index_meta.resolver_environment TEXT`. `analyze`
(`crates/loomweave-cli/src/analyze.rs`) computes it via
`loomweave_core::resolver_environment_for(&plugin.manifest, &project_root)`,
which is `Some(fingerprint)` only for manifests declaring
`[capabilities.runtime.pyright]` and `None` for every other plugin.
`ProjectInterpreter::fingerprint()` renders:

- a pinned path verbatim (its display string);
- an unpinned-but-found path as `unpinned:<path>` — so a `path`-rung guess
  can never compare equal to a pinned choice that happens to share the same
  bytes (a venv appearing later at the same location the guess landed on
  still moves the marker);
- `unpinned:none` when nothing was found.

`resolver_environment_changed` compares the current fingerprint against the
prior run's stored marker: `Some(prior) => prior != current`; `None` (no
stored marker at all) `=> current.is_some()`, so a non-pyright plugin is
never dragged into a re-dispatch by a signal that can never apply to it. A
change — or a NULL prior marker, i.e. an index built before this migration —
forces the same full re-dispatch of that plugin's files as a plugin/ontology
version bump (`plugin_index_contract_changed` in `analyze.rs`), and is logged
by name (`resolver environment moved since last run`, both fingerprints).

### 5. The host exports the interpreter only when pinned and unset

`PluginHost::spawn_unhandshaken` calls `exported_interpreter` (`host.rs`)
before launching a pyright-capable plugin child, and sets
`LOOMWEAVE_PYTHON_INTERPRETER` in the child's environment only when all three
guards pass:

1. the manifest declares `[capabilities.runtime.pyright]` — nothing else
   consumes the variable;
2. the operator has not already set a NON-EMPTY `LOOMWEAVE_PYTHON_INTERPRETER`
   in the analyze process's own environment — an existing value already wins
   the plugin's own discovery (rung 1), and overwriting it would silently
   ignore an explicit operator pin. An empty value is "unset" here exactly as
   it is on the discovery rung above, so the host still exports;
3. the host's own discovery landed on a **pinned** rung. A bare `PATH` guess
   is no better than the plugin's own fallback, and exporting it would
   present a guess to the plugin as an authoritative pin.

Both `analyze`'s fingerprint computation and `spawn_unhandshaken`'s export
canonicalise `project_root` before running discovery, and discovery itself is
**not** root-invariant (it joins `.venv/bin/python` onto the root as given and
only lexically normalises) — so both call sites must canonicalise, or the
recorded marker skews against the exported interpreter and every run
re-dispatches (pinned by a dedicated Rust unit test,
`the_root_canonicalisation_at_both_call_sites_is_load_bearing`).

**Agreement today is by construction, not by a shared check.** The
`plugin_index_meta.resolver_environment` fingerprint records the **host's**
discovery even on the unpinned path, where the host exports nothing and the
plugin discovers on its own. The host and plugin agree in that case only
because the plugin child inherits the host's environment and the host passes
the same canonical project root — there is no cross-process assertion that
their answers actually match. A byte-identical cross-language conformance
fixture that exercises both discovery implementations against the same
environment vectors is a named follow-up, not shipped here.

### 6. `loomweave doctor` — `index.runs` and the `index.resolution_coverage` remedy

`index.resolution_coverage`'s `--fix` remedy text now names
`interpreter_unpinned` explicitly alongside the content-determined tokens,
and points the operator at the fix: "set `LOOMWEAVE_PYTHON_INTERPRETER` or
create `.venv`" (`crates/loomweave-cli/src/doctor.rs`,
`default_next_action`). Separately, this ticket also shipped a new
`index.runs` check: `runs` rows a builder left `running` (OOM-kill,
`kill -9`, reboot) poison `project_status_get` and the hook snapshot until
`analyze`'s own 24-hour stale sweep catches them. `doctor` now detects them
directly — using the analyze lock itself as the liveness proof: if `doctor`
can acquire it, no builder is alive, so every `running` row is abandoned —
and `--fix` marks them `failed`. This is operational hygiene bundled into the
same ticket, not part of the interpreter-discovery mechanism itself.

## Consequences

- **The first incremental run after upgrading re-dispatches every Python file
  once.** An index built before migration `0015` has no
  `resolver_environment` marker (`NULL`), which reads as "changed" the same
  way a `None` prior marker does, so `plugin_index_contract_changed` trips
  for every pyright-capable plugin on the first post-upgrade run. This heals
  rows pinned by the pre-fix launcher-dependent behaviour, at the cost of one
  full-plugin re-dispatch.
- **A venv-less project with a flapping `PATH`** re-dispatches on every flip:
  each time the first `python`/`python3` on `PATH` resolves to a different
  file, the `path`-rung fingerprint (`unpinned:<path>`) changes and forces
  another full re-dispatch of that plugin's files. Pinning a `.venv` or
  setting `LOOMWEAVE_PYTHON_INTERPRETER` removes the flap by moving discovery
  onto a pinned, stable rung.
- **Rebuilding `.venv` in place does not move the fingerprint.** The marker is
  the interpreter PATH (never symlink-resolved — see §1), so recreating the
  virtualenv on a different base Python leaves `.venv/bin/python` at the same
  path. `plugin_index_contract_changed` does not trip, the incremental
  content-hash skip holds, and evidence resolved against the OLD base
  interpreter stays pinned until a `--no-incremental` pass. Folding the
  symlink's target (or the venv's `pyvenv.cfg`) into the fingerprint is a
  possible follow-up; it was not taken here because it would re-dispatch on
  every base-interpreter patch bump.
- **Windows `Scripts\python.exe` layouts are not covered.** Every rung above
  joins `bin/python` (Unix venv/conda layout); a Windows venv's
  `Scripts\python.exe` is not discovered by any rung. Windows operators need
  the `LOOMWEAVE_PYTHON_INTERPRETER` override.
- **This repository's own Python plugin tests read `interpreter_unpinned`
  under a bare `loomweave analyze` at the repo root.** The repository root
  has no `.venv` (verified: `/home/john/loomweave/.venv` does not exist) —
  the plugin's own venv lives at `plugins/python/.venv` instead — so rung 2
  (`<project_root>/.venv/bin/python`) does not find it. Absent an activated
  venv or conda env in the launching shell (`VIRTUAL_ENV`/`CONDA_PREFIX`
  unset), discovery falls through to the `path` rung or further, and the
  facet demotes. The operator remedy is the same as any other project:
  `LOOMWEAVE_PYTHON_INTERPRETER=$(pwd)/plugins/python/.venv/bin/python`.
- **The external SQLite read ceiling moved to 15 as a side effect, not a
  deliberate widening.** `CURRENT_SCHEMA_VERSION` bumped to 15 for migration
  `0015`, and `EXTERNAL_READ_MAX_USER_VERSION` (ADR-055) was bumped to 15 in
  the same commit. Migration `0015` touches only `plugin_index_meta`, which
  is not part of ADR-055's safe projection, so the advance is a genuine no-op
  for external readers today. It is still worth naming explicitly: ADR-055's
  rule that the external maximum is "not an alias of `CURRENT_SCHEMA_VERSION`"
  and that advancing it "requires reviewing the safe projection" is enforced
  by author discipline and a `const _: () = assert!(...)` ordering check, not
  by any tooling that verifies the projection was actually reviewed before
  the constant moved. A future migration that touches a projected table could
  land the same way.

## Alternatives considered

- **Fix the launcher hook to put the venv on `PATH`.** Rejected: it cures one
  launcher. Every other way `analyze` gets invoked — a different agent hook,
  CI, an operator's raw shell, `loomweave worktree analyze` — carries its own
  `PATH` and would need the identical fix repeated. Discovery inside the
  plugin (and the host) fixes every launcher at once.
- **Explicit `extraPaths` in the pyright configuration.** Unnecessary: once
  pyright is pointed at the project's own interpreter via `pythonPath`, it
  resolves the project's own `site-packages` (including an editable install)
  without needing a separately maintained search-path list.
- **`transient=true` for `interpreter_unpinned`.** Rejected. A transient claim
  spends the file's re-dispatch budget (`redispatch_attempts`,
  `MAX_REDISPATCH_ATTEMPTS`) on retries that cannot possibly recover: re-running
  analyze with the same unpinned interpreter reproduces the identical hole
  every time. Marking it `transient=false` keeps it out of that budget
  entirely; healing comes from the `resolver_environment` marker forcing a
  full re-dispatch when the interpreter fingerprint actually changes (e.g. an
  operator creates `.venv` or sets `LOOMWEAVE_PYTHON_INTERPRETER`), which is
  the only event that can actually fix the hole.
- **A `loomweave.yaml` key for the interpreter choice.** Rejected: the single
  override surface is the `LOOMWEAVE_PYTHON_INTERPRETER` environment variable
  — read by the plugin directly and (for pyright-capable plugins, subject to
  the three guards in Decision 5) exported by the host. This keeps the
  override where an operator's shell/CI/hook environment can set it
  per-invocation without touching a committed config file, and keeps the
  single literal (`INTERPRETER_OVERRIDE_ENV` / `PYTHON_INTERPRETER_ENV`) as
  the one place the cross-language contract can drift if renamed on only one
  side.

## Amendment (2026-09-02) — rung-2 trust condition

**Change.** Rung 2 (`<project_root>/.venv/bin/python`, source `dotvenv`) is
accepted only when the path is **not repository content**: `tracked_state`
(ADR-063) must answer `untracked`, `not_a_git_work_tree`, or
`git_unavailable`. On `tracked` or the fail-closed `unknown` the rung is
skipped exactly as if the file were absent, and discovery continues with
`VIRTUAL_ENV` → `CONDA_PREFIX` → `PATH` → none. `git_unavailable` (no `git`
binary resolvable at all) is deliberately NOT fail-closed: a missing `git` is
the operator's environment, not repository content — `PATH` is
real-environment-only (ADR-062) — so the rung is accepted as if `git` had
answered `untracked`. Both sides of the cross-language contract
(`interpreter.rs`, `interpreter.py`) apply the same predicate and log the
skip once per process.

**Why.** pyright executes `python.pythonPath`. A repository that commits an
executable at `.venv/bin/python` — or a symlink at `.venv` to committed
content — gets code execution as the operator on the first `analyze`,
including the hook-spawned background one (Codex #142, closed; clarion-9b3cf287b7).
An operator-created venv is always untracked, so no rung-2 hit is lost to the
`tracked` verdict. The fail-closed `unknown` verdict can lose a legitimate
hit — an `ls-files` probe that times out, or a checkout owned by another uid
that git refuses as `dubious ownership` under the hardened environment (the
operator's `safe.directory` is deliberately not consulted; foreign ownership
is itself an untrusted-corpus signal). A missing `git` binary is the
operator's environment, not repository content, and reads `git_unavailable`,
which keeps the rung. Every skip is surfaced by the once-per-process warning
and by the existing `interpreter_unpinned` token. `pyrightconfig.json`
`venvPath`/`venv` are not gated: verified against the pinned pyright bundle,
those keys only shape site-packages globbing and never execute a program.

**What this does not change.** Rungs 1 and 3–6, the `access(2)` executability
ruling, lexical normalisation, the fingerprint, the host-export guards, and the
`interpreter_unpinned` semantics are untouched. A skipped rung 2 that lands on
rung 5/6 is reported through the existing `interpreter_unpinned` token; `doctor`
names the fix (`git rm --cached .venv` is the wrong fix — the operator should
create their own venv; the committed one is the repository's business).
