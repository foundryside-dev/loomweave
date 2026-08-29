# ADR-057: Pyright Restart Attribution and the Sticky Self-Inflicted Mark

**Status**: Accepted
**Date**: 2026-08-29
**Deciders**: john
**Extends**: [ADR-050](./ADR-050-plugin-lifecycle-deadlines.md)
**Tickets**: clarion-3e517d4aff, clarion-7f527d3d32, clarion-7fc41105ea

## Context

The Python plugin resolves calls and references through a long-lived
`pyright-langserver` subprocess. That process can die (a file it cannot
type-check crashes it), stall (a query exceeds the file's budget), or be
disabled for the rest of the run once a restart cap trips. Each file's
`analyze_file` result carries a per-facet coverage claim
(`status`, `reason`, `transient`, `collateral`) so the host can re-dispatch
degraded files next run and name the hole on its read surface
(clarion-3e517d4aff).

Live evidence on elspeth (2026-08-29) showed the pre-decision attribution
was wrong in a way that made the degraded set rotate run to run: one
incremental run re-dispatched 447 files and produced 2 self-inflicted rows
plus 443 collateral rows. Each crashing file cost two run-level restarts
(one on arrival, because the previous crash was only discovered when the
next file opened its document, and one mid-request), a run-level cap of 3
was exhausted by two files, every later file came back empty, and next run
the poisoned troublemakers — now marked collateral — were dispatched early
and poisoned a different tail.

The plugin subprocess inherits the host's environment, and the host's
per-file watchdog (`DEFAULT_PLUGIN_FILE_TIMEOUT`, ADR-050) is overridable via
`LOOMWEAVE_PLUGIN_FILE_TIMEOUT_MS`. Any restart the plugin performs inside a
file's call must fit under that deadline or the host kills the whole plugin
subprocess, losing every queued file.

## Decision

### 1. Who broke the resolver is decided by the catch-site, never by message text

`collateral = false` (**self-inflicted**) means exactly one of:

- `pyright_timeout` — this file's own per-query or per-file budget expired
  while pyright stayed alive.
- `pyright_transport_failure` — pyright died while **this file's request was
  in flight** (after `didOpen`, inside the per-function / per-site loop).
- `pyright_local_read_error` — an `OSError` reading an unrelated target file
  mid-pass while pyright is confirmed alive. Not a pyright-health event, but
  it is this file's evidence gap, so it does not exonerate the file.

Everything else is `collateral = true`:

- `pyright_restarting` — pyright was already dead when this file's
  `didOpen` was sent (it died after answering the previous file).
- `pyright_spawn_failed` — a transient spawn deferral left this file without
  a process.
- `pyright_unavailable` / `pyright_poisoned` / `pyright_restart_cap_exceeded`
  — the run is disabled; `PyrightRunState.disabled_reason` is the single
  authority on which token applies.

Content-determined tokens (`syntax_error`, `reference_site_cap`) carry
`transient = false` and are outside the attribution question.

### 2. A file-attributed crash restarts pyright immediately and is not charged to the run-level cap

When pyright dies during a file's own request the session respawns it before
returning that file's result, so the next file arrives at a live process.
Such restarts increment `file_attributed_restart_count`, not
`restart_count`, and therefore do not spend `MAX_PYRIGHT_RESTARTS_PER_RUN`
(3), which now guards only deaths **not** attributable to a file
(dead-on-arrival with no preceding self-inflicted abort, crash loops).

A separate safety budget bounds **all** restarts:
`MAX_TOTAL_PYRIGHT_RESTARTS_PER_RUN` (25) or
`MAX_PYRIGHT_RESTART_LATENCY_BUDGET_MS` (240 s of cumulative spawn+initialize
time). Tripping it disables the run with `pyright_restart_cap_exceeded` and
emits `LMWV-PY-PYRIGHT-TOTAL-RESTART-CAP`.

**Deviation from the ticket text**: a single `pyright_timeout` does **not**
restart pyright. A timeout means the process is alive but slow; respawning
would discard its warm program cache and buy nothing. The file is still
self-inflicted.

A **streak** is different. A pyright that hangs on every query still
answers `poll()` as alive, so no death is ever observed, no restart is ever
attempted, and every remaining file in the run times out and is pinned
self-inflicted. The session therefore keeps a consecutive-timeout breaker
(`PyrightRunState.consecutive_timeout_files`): it counts a file once per
calls pass whose coverage lands on `pyright_timeout`, and resets whenever a
calls pass completes or a process is spawned. Reaching
`MAX_CONSECUTIVE_TIMEOUT_FILES` (3) treats the process as wedged and
restarts it **once**, immediately, before the streak-closing file's result
is returned, through the same headroom-aware respawn path an in-flight
death uses (so it defers to the next file at the watchdog ceiling rather
than overrunning it). The restart is charged to the run-level
`restart_count` and checked against the total safety budget, so a wedge
that persists across restarts is bounded exactly like a crash loop and ends
with `pyright_poisoned` (or `pyright_restart_cap_exceeded`), never in an
unbounded restart loop. It emits `LMWV-PY-PYRIGHT-WEDGED-RESTART` anchored
to the streak-closing file, naming the streak length. The streak's files
keep their honest self-inflicted `pyright_timeout` claim: they are
re-dispatched next run against a fresh process and heal if they complete.

### 3. The restart respects the host's per-file watchdog, including its env override

The session reads `LOOMWEAVE_PLUGIN_FILE_TIMEOUT_MS` (the same variable the
host honours; default 120 s) and never extends a crashing file's deadline
past `first touch + watchdog − 15 s` (or half the watchdog when it is too
short for the full margin). Whether to respawn is decided on the **real**
headroom `ceiling − now` at the moment pyright dies — never on the file's
deadline, which is anchored at file start and reduces a `deadline >=
ceiling` check to `budget >= window`, blind to how late in the window the
crash landed. Below `MIN_RESPAWN_HEADROOM_SECS` (5 s) the respawn is
**deferred** to the next `_ensure_process`; otherwise it is attempted with
its initialize handshake bounded to the headroom, so a hung respawn is cut
off inside the window and deferred (not treated as `pyright_unavailable`)
instead of overrunning the host's real deadline and getting the whole
plugin call killed.

A deferral records which file it was charged to
(`restart_charged_to_path`). That file's **own** later facet in the same
window (references after calls) does not consume the deferral: it reports
`pyright_transport_failure` self-inflicted with no respawn and no second
restart event, because the window the deferral found exhausted is the same
one. The one-shot `restart_already_charged_to_file` flag is consumed by
the next spawn for a *different* file — including the first spawn of a
freshly recycled `PyrightSession` — silently: no finding, no run-level
charge, no collateral mark. It can never linger and later mask a genuine
arrival-death.

### 4. The self-inflicted mark is sticky in storage

`upsert_source_file_resolution_coverage` keeps `collateral = 0` when the
prior row for the facet was self-inflicted (`degraded && transient &&
!collateral`) and the new claim is transient-degraded collateral: being
swept into another file's poison window does not exonerate a known
troublemaker, which the host must keep dispatching last. The facet's
`reason` travels with the mark: the prior self-inflicted token is carried
forward, so a row never says `collateral = 0` beside `pyright_poisoned`.
The mark un-sticks
when the new claim is `complete`, is content-determined, or the file's bytes
changed. The host raises `content_changed` from its whole-file hash
consultation on every dispatch path — the contract-change full re-dispatch
included — and `--no-incremental` has no prior hashes, so it reports every
file as changed; that is the operator's documented remedy for a wrongly
stuck mark. `redispatch_attempts` is untouched by the override.

### 5. Findings are anchored to the arriving file and say so

`LMWV-PY-PYRIGHT-RESTART` / `LMWV-PY-PYRIGHT-POISON-FRAME` findings carry
`attribution: "in_flight" | "arrival"`. An arrival finding is anchored to the
file that discovered the death and its message says the death happened
after the previous file's request completed. Restart findings are never
deduplicated away.

## Consequences

- On a corpus with two crashing files the two stay self-inflicted and every
  file behind them resolves against a live process: the collateral count
  after one incremental run is expected to be zero, and the self-inflicted
  set is identical across consecutive runs.
- `runs.stats` gains `pyright_restart_count`,
  `pyright_file_attributed_restart_count`,
  `pyright_ceiling_deferred_restart_count` and
  `pyright_init_latency_total_ms` so an operator can see restart spend.
- Raising `LOOMWEAVE_PLUGIN_FILE_TIMEOUT_MS` widens the in-process restart
  window; lowering it makes the session defer earlier. Neither can push a
  restart past the host's real deadline.
- The reason vocabulary above is the documented one; `pyright_session.py`'s
  module docstring mirrors it and must be kept in sync with this ADR.

## Operational note

An incremental run re-dispatches only rows below `MAX_REDISPATCH_ATTEMPTS`
(`files_needing_resolution_redispatch` filters on `redispatch_attempts`).
Rows that already exhausted their attempts under the pre-ADR attribution
are therefore **not** picked up by an incremental analyze after upgrading.
The acceptance / heal recipe is `loomweave doctor --fix` (resets exhausted
rows, PR #118) followed by an incremental `loomweave analyze`; a
`--no-incremental` pass is the heavier alternative, since it reports every
file as changed and un-sticks every mark.

**Per-request grant (2026-08-29, clarion-5d83413c36).** `PYRIGHT_CALL_TIMEOUT_SECS` is 30 s, not 5 s: the first query on a large file pays for pyright's whole-file analysis and routinely exceeded 5 s, which read as a self-inflicted `pyright_timeout` although the file completes in ~11 s once warm. The effective grant is still `min(30 s, remaining file budget)`, so a truly wedged server is still detected — the three-file wedge breaker above still counts files, not requests, and the mechanism is unchanged — but time-to-detect scales with the grant: worst case is now ~180 s (3 files x 2 passes x 30 s) instead of the pre-change ~30 s.

## Alternatives considered

- **Restart on timeout too** (the ticket's literal grouping). Rejected: a
  live-but-slow pyright is the common shape for large typed files; the
  respawn costs 10–30 s of initialize and the warm cache.
- **Keep one run-level cap for all restarts.** Rejected: it is exhausted by
  two troublemakers and poisons the rest of the run — the elspeth failure.
- **Trust the plugin's collateral claim verbatim.** Rejected: the poisoned
  tail exonerates the troublemaker and rotates the self-inflicted set.
