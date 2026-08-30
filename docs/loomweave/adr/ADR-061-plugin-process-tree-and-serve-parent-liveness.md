# ADR-061: Plugin Process-Tree Kill and `serve` Parent-Liveness Exit

**Status**: Accepted
**Date**: 2026-08-31
**Deciders**: john@foundryside.dev
**Context**: clarion-ebf404dfbb (salvage worklist B2). Observed on a shared checkout: 14 `loomweave serve` processes plus 3 defunct at once (one per MCP client, never reaped), an orphaned pyright at 103 % CPU / 2.5 GB for 1 h 40 m with `ppid 1` and no analyze running, and two `runs` rows stuck `running` with dead owner pids. The third symptom was already handled by ADR-058's `doctor --fix` (`index.runs` repairs abandoned rows under the analyze lock) and by `analyze` marking stale running rows failed at start; this ADR records the fixes for the first two.

## Summary

Every host kill of a plugin child takes the child's **whole process tree**: descendants are collected from `/proc/<pid>/task/*/children` *before* the kill (they are reparented the instant the child dies), then child and descendants are SIGKILLed. The Python plugin does the same to its `pyright-langserver` wrapper's subtree on every restart/close. `serve` polls its parent pid (`getppid`, a safe std call) every ~2 s inside the existing supervision loop and, on a reparenting, shuts the HTTP read API down exactly as the SIGTERM path does and exits 0. No process groups, no `PDEATHSIG` from Rust, no heuristic adoption of orphans.

## Context

- The venv's `pyright-langserver` is a Python entry-point wrapper that runs **node** as a grandchild. The plugin already sets `PDEATHSIG=SIGTERM` on the wrapper, and `_terminate_process` killed the wrapper — but nothing ever killed node. A healthy node exits on stdin EOF; a *wedged* node (the very case that triggers a restart or a watchdog kill) does not, and that is the 103 % CPU orphan.
- `serve` already exits when its stdin reaches EOF. The accumulated `serve`s belonged to clients that never closed stdin (the Codex app-server pattern: MCP children accumulate holding pipes open). EOF cannot detect that; parent death can.
- The workspace denies `unsafe_code` except the `pre_exec`/`setrlimit` block; `prctl(PR_SET_PDEATHSIG)` from Rust would need unsafe FFI.

## Decision

1. **Tree kill in the host.** `loomweave_core::kill_process_tree(&mut Child)` (`plugin/process_tree.rs`) is used at every host kill site: handshake failure in `PluginHost::spawn`, the watchdog kill, `LivePlugin::teardown_after_kill`, the shutdown-fallback kill, and the reap-timeout kill. Descendants via `/proc` on Linux (`CONFIG_PROC_CHILDREN`, on by default); elsewhere the walk is empty and only the direct child is killed — the pre-existing behaviour. Signals go through `nix`'s safe `kill` wrapper.
2. **No process group for the plugin.** `process_group(0)` would let a group kill cover macOS too, but it also stops a terminal's Ctrl-C from reaching the plugin alongside `analyze` — orphaning it in exactly the interactive case that works today. Rejected.
3. **Plugin-side subtree kill.** `PyrightSession._terminate_process` walks the wrapper's descendants (same `/proc` walk) before `kill()` and SIGKILLs them after; `PDEATHSIG` on the wrapper stays.
4. **`serve` parent liveness.** `supervise_stdio_http_and_signals` takes a `parent_alive` probe; production compares `parent_id()` to the value at startup every `PARENT_PROBE_EVERY_TICKS` (20 × 100 ms). A reparenting yields `SupervisedOutcome::ParentGone`: HTTP read API shut down (its `PublishedPortGuard` compare-and-deletes the `ephemeral.port` marker), `exit(0)` without joining the stdin-parked stdio thread.
5. **No orphan adoption at `analyze` start.** Killing `pyright-langserver` processes matched by cwd/cmdline could hit a developer's own editor pyright for the same project. Rejected; the tree kill removes the source of orphans instead.

## Consequences

### Positive

- A watchdog kill or a pyright restart no longer leaves a node spinning at `ppid 1`; a dead MCP client no longer leaves a `serve` idling with an `ephemeral.port` marker published.
- Pinned by tests that scan `/proc/*/environ` for a per-test marker the grandchild inherits (`watchdog_kill_takes_the_plugin_grandchild_too`), a core unit test on a real `sh → sleep` tree, a Python test on `_terminate_process`, and a supervisor test with the probe reporting the parent gone.

### Negative

- Non-Linux hosts keep the old direct-child-only kill (no `/proc`); the plugin's `PDEATHSIG` and stdin EOF remain the only grandchild controls there.
- A `serve` launched by a supervisor that intentionally re-parents children (a subreaper that hands off) would exit; none of the supported launch paths do that.

## Related Decisions

- **Related to**: [ADR-050](./ADR-050-plugin-lifecycle-deadlines.md) (the kills this ADR makes complete), [ADR-057](./ADR-057-pyright-restart-attribution.md) (plugin-side restart discipline), [ADR-058](./ADR-058-project-interpreter-discovery.md) (`doctor` `index.runs` repair), [ADR-060](./ADR-060-git-sync-coalescing-and-fast-path.md) (`LivePlugin` respawn).
