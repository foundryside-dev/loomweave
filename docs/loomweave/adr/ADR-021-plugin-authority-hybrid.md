# ADR-021: Plugin Authority Model — Hybrid (Declared Capabilities + Core-Enforced Minimums)

**Status**: Accepted
**Date**: 2026-04-18
**Deciders**: qacona@gmail.com
**Context**: plugin trust posture between "trusted extension" (status quo before this ADR) and "full sandbox" (deferred to v0.2)

## Summary

Plugins run as subprocesses at the user's UID (ADR-002). The v0.1 authority model is **hybrid**: the plugin declares its expected runtime envelope in the manifest (capabilities, RSS expectation, entity-emission shape), and the core unconditionally enforces a small set of minimum-safe controls at the transport and pipeline layer — a path jail refusing plugin-returned paths outside the project root, a Content-Length ceiling on every JSON-RPC frame, a per-run entity-count cap, and a per-plugin RSS limit applied via `prlimit`/`setrlimit` at spawn. Violations kill the plugin, emit `LMWV-INFRA-PLUGIN-VIOLATION` with a specific subcode, and participate in the crash-loop circuit breaker. This is not a sandbox (no seccomp/landlock); it is a set of non-negotiable guardrails that turn "trusted-source-only" from a doctrine into an enforced floor.

## Context

ADR-002 specifies plugin transport (Content-Length-framed JSON-RPC over a subprocess) but leaves the authority posture unstated. The default inherited from the detailed design (before this ADR) is "trusted extension" — plugins are PyPI packages installed via `pipx`; the core trusts them fully and runs them at the operator's UID. The 2026-04-17 panel's threat-model review (`reviews/panel-2026-04-17/09-threat-model.md` §7) scored four threats that this posture leaves open:

- **T-01** (risk 9, compound-critical) — plugin = arbitrary code execution at user UID via supply-chain compromise or hostile third-party plugin.
- **T-08** (risk 6) — plugin path traversal / symlink escape from project root; `file_list` is advisory, no core-side path jail exists.
- **T-11** (risk 4) — framing / size DoS on JSON-RPC; no explicit Content-Length cap.
- **T-12** (risk 4) — entity-count / finding-count DoS from a malicious plugin; no per-run bound, writer actor saturates.

The panel's "three non-negotiable v0.1 controls" (`09-threat-model.md` §12, recommendation 2) names these four specifically and prescribes path jail + per-run entity/message caps + Content-Length ceiling + per-plugin ulimit RSS as the minimum enforcement needed before release. The design already names "plugin hash-pinning deferred to v0.2" (NG-16) — this ADR does not reopen that; it closes the four threats that *do not* depend on hash-pinning.

Against this the case for a full sandbox (seccomp, landlock) is real but costs more than its marginal benefit at v0.1. Python plugins import `wardline.core.registry` at startup (`system-design.md:949`), which walks `sys.path` — a landlock ruleset refusing unexpected opens breaks the reference plugin. Cross-platform coverage is uneven (seccomp Linux-only; landlock kernel ≥5.13). The first-party Python plugin is the only v0.1 plugin; the sandbox cost buys security against a threat that hash-pinning (v0.2) will close more durably.

The scope-commitment memo (Q3, `v0.1-scope-commitments.md`) picked this hybrid explicitly: declared capabilities + core-enforced minimums, not full sandbox.

## Decision

The plugin authority surface has three layers.

### Layer 1 — Plugin manifest declarations (plugin's self-description)

The manifest (`detailed-design.md:60-136`) gains a `capabilities.runtime` block that declares the plugin's expected envelope. These values are declarations, not enforcements — the core uses them for sanity-warnings and to pick a floor that is no stricter than what the plugin asked for.

```yaml
capabilities:
  ...existing confidence_basis / edge_extraction declarations...
  runtime:
    expected_max_rss_mb: 1500          # plugin's own estimate
    expected_entities_per_file: 5000   # for sanity-warning against misconfigured glob
    wardline_aware: true               # plugin reads wardline.core.registry.REGISTRY
    reads_outside_project_root: false  # opt-out declaration; if true, plugin must
                                       # enumerate which paths (not supported in v0.1)
```

A plugin declaring `reads_outside_project_root: true` in v0.1 is refused at `initialize` with `LMWV-INFRA-MANIFEST-UNSUPPORTED-CAPABILITY`; v0.1 has no mechanism to allow it.

### Layer 2 — Core-enforced minimums (non-negotiable, applied to every plugin)

These four controls are applied to every plugin unconditionally, regardless of manifest content. They are the enforcement floor this ADR commits to.

**2a — Path jail.** Every path the plugin returns (from `file_list`, every `file_analyzed` notification's `source.file_path`, every evidence anchor) is normalised via `std::fs::canonicalize` (follows symlinks) and checked against `project_root`. A path resolving outside `project_root` is dropped (the entity/edge/finding it anchors is refused), and `LMWV-INFRA-PLUGIN-PATH-ESCAPE` is emitted with `metadata.loomweave.offending_path` set to the rejected input. The plugin is **not** killed on first violation — the core treats path escape as a correctness bug more often than a live attack — but the crash-loop circuit breaker counts repeated escapes at a lower threshold (>10 escapes in 60s → plugin killed, `LMWV-INFRA-PLUGIN-DISABLED-PATH-ESCAPE`).

**2b — Content-Length ceiling.** Every inbound JSON-RPC frame from the plugin has a Content-Length header (ADR-002). A frame exceeding the ceiling is a framing error. Default ceiling: **8 MiB** per frame (configurable via `loomweave.yaml:plugin_limits.max_frame_bytes`, floor 1 MiB). The framing parser refuses the frame before deserialising; the plugin is killed with SIGTERM → SIGKILL if non-responsive; `LMWV-INFRA-PLUGIN-FRAME-OVERSIZE` is emitted with `metadata.loomweave.observed_bytes` and `metadata.loomweave.ceiling_bytes`. Crash-loop counter increments.

**2c — Entity-count cap.** Per-run cumulative cap on `entity` + `edge` + `finding` notifications from a single plugin. Default: **500,000** combined records (configurable via `loomweave.yaml:plugin_limits.max_records_per_run`, floor 10,000). On exceed: the current in-flight batch is flushed to the store; the plugin is killed; `LMWV-INFRA-PLUGIN-ENTITY-CAP` is emitted. The run continues to Phase 2 (write-actor drains remaining queued records) but enters a partial-results state that forces `--force` to overwrite.

**2d — Per-plugin RSS limit.** Applied at spawn via `prlimit(RLIMIT_AS)` on Linux, `setrlimit(RLIMIT_AS)` on macOS (POSIX path). Default: **2 GiB** virtual-memory cap (configurable via `loomweave.yaml:plugin_limits.max_rss_mib`, floor 512 MiB). Process killed by OS on cap exceed; the core detects the SIGKILL exit (`WIFSIGNALED && WTERMSIG == 9`) and emits `LMWV-INFRA-PLUGIN-OOM-KILLED`. Crash-loop counter increments.

The four subcodes of `LMWV-INFRA-PLUGIN-VIOLATION` — `PATH-ESCAPE`, `FRAME-OVERSIZE`, `ENTITY-CAP`, `OOM-KILLED` — are plugin-independent and surface in every compat report.

### Layer 3 — Crash-loop circuit breaker interaction (ADR-002 carries through)

ADR-002 defines a crash-loop breaker (>3 crashes in 60s → plugin disabled for the run). Violations from Layer 2 count as crashes. The breaker's existing finding (`LMWV-INFRA-PLUGIN-DISABLED-CRASH-LOOP`) is augmented: when the triggering crashes are all Layer 2 violations of the same subcode, the disabled-finding carries `metadata.loomweave.disabled_reason` with the dominant subcode. This is a diagnostics improvement, not a policy change.

### What is NOT in Layer 2 (explicit non-defences)

- **Seccomp/landlock syscall sandbox.** Deferred to v0.2 with NG-16 (plugin hash-pinning). The reference Python plugin's `wardline.core.registry` import needs unconstrained `sys.path` access; a seccomp-tight ruleset breaks the reference plugin.
- **Per-plugin CPU cap.** Plugin crash-loop breaker already catches runaway loops; explicit CPU cap adds complexity without closing a scored threat.
- **Outbound-network ACL on the plugin.** Python plugins may legitimately read package metadata at runtime; a default-deny network policy breaks ergonomics for a threat class (plugin exfiltration) that is better closed by hash-pinning (v0.2).
- **Per-RPC rate limiting.** Crash-loop breaker + entity-count cap cover the DoS surface at v0.1 scale.

## Alternatives Considered

### Alternative 1: Full sandbox (seccomp + landlock)

Run plugins with a default-deny seccomp filter plus a landlock filesystem ruleset restricting reads to `project_root` and the plugin's own install path.

**Pros**: strongest isolation story; neutralises T-01 materially even before hash-pinning lands in v0.2.

**Cons**: the reference Python plugin's `import wardline.core.registry` needs `sys.path` traversal outside the project root (pipx venv at `~/.local/pipx/venvs/...`); a landlock ruleset tight enough to be meaningful breaks the reference plugin. Cross-platform coverage is uneven (Linux kernel ≥5.13 for landlock; macOS has no equivalent surface). Engineering cost is large — ruleset tuning is per-plugin, and the core becomes responsible for expressing "allow the imports Python needs to import anything in `sys.path`" as a filesystem policy. Diagnostics are opaque (sandbox-denied syscalls surface as `EACCES` or `EPERM` from library code, not as a Loomweave-owned finding).

**Why rejected**: for a v0.1 shipping with one first-party plugin, hybrid closes the enumerated threats at ~10% of the engineering cost, and v0.2 adds hash-pinning which is a more durable answer to T-01 than syscall filtering. Sandbox is the right v0.2+ direction; it is the wrong v0.1 investment.

### Alternative 2: Trusted-extension (status quo, no ADR)

Leave the plugin boundary unenforced. Trust the plugin as much as any other third-party dependency.

**Pros**: zero engineering cost; no new surface for plugins to fail validation on.

**Cons**: T-01 at risk 9, T-02 at risk 9, T-08/T-11/T-12 unaddressed. The 2026-04-17 panel's "three non-negotiable v0.1 controls" (`09-threat-model.md` §12) names this outcome as inconsistent with shipping a security tool. A single typo-squatted plugin in a v0.1 release cycle exfiltrates source to an attacker; the product's marketing claim ("local-first, audit-ready") becomes false in the first week.

**Why rejected**: the hybrid controls are individually cheap, the panel named the specific gaps, and the reputational failure mode of shipping a security tool with trivially-exploitable plugin DoS is disproportionate.

### Alternative 3: Manifest-only (plugin declares limits; core trusts declarations)

The manifest declares `expected_max_rss_mb`, `max_entities_per_run`, `max_frame_bytes`, and `reads_outside_project_root`. The core uses those declared limits as its enforcement floor — effectively, the plugin sets its own guardrails.

**Pros**: respects plugin authorship; avoids "one size fits all" tuning debates.

**Cons**: a malicious plugin declares generous limits (50 GiB RSS, 10 M records, no path jail) and operates within them. The threat model requires core-side enforcement that is independent of the manifest. Declarations alone are worse than a fixed floor because they telegraph weakness — "this plugin declares it can write anywhere" is a contract nobody wants to sign.

**Why rejected**: enforcement must be independent of the plugin's own claims for the four named threats to actually close.

### Alternative 4: cgroup v2 resource limits instead of prlimit

Use cgroup v2 (`systemd-run --user --scope` or direct cgroup mounts) for per-plugin memory, CPU, and IO limits.

**Pros**: richer than `RLIMIT_AS` — separate memory / swap / IO limits; structured accounting.

**Cons**: cgroup v2 cross-distro setup is inconsistent (rootless-cgroup support, v1/v2 mixed-mode systems, macOS has no cgroups at all). `prlimit`/`setrlimit` works on every POSIX target Loomweave will ship to (Linux, macOS, Windows via WSL). For the specific control — "kill the plugin if it allocates too much memory" — `RLIMIT_AS` is sufficient.

**Why rejected**: incremental control richness is not worth the cross-platform surface area at v0.1 scale.

## Consequences

### Positive

- Four threats rated by the panel as non-negotiable for v0.1 are closed: T-08 (path traversal), T-11 (framing DoS), T-12 (entity-count DoS), with T-01 partially mitigated (sandbox defers to v0.2 with hash-pinning, but resource exhaustion and path escape — two of T-01's exploitation paths — are closed).
- "Trusted-source-only" stops being a doctrine and becomes an enforced minimum. The panel's framing ("configuration and enforcement changes, not new subsystems") is satisfied.
- The manifest's `capabilities.runtime` block is an extension point. v0.2's hash-pinning (NG-16) and sandbox story can read and enforce more of it without redesigning the authority model.
- Every violation is a finding. Operators run `filigree list --label=loomweave-infra --rule-id=LMWV-INFRA-PLUGIN-VIOLATION` to audit plugin behaviour across runs.
- Defaults are conservative but tunable. Teams with unusually large codebases raise `max_records_per_run` in `loomweave.yaml`; teams on memory-tight hosts lower `max_rss_mib`. The ADR names the configuration keys.

### Negative

- Plugin authors have one more contract to satisfy — the four limits are real and can bite a plugin that emits millions of noisy `LMWV-FACT-*` findings. Mitigation: the `expected_entities_per_file` manifest declaration produces a sanity-warning (`LMWV-INFRA-PLUGIN-ENTITY-OVERRUN-WARNING`) well before the hard cap, so the first sign of trouble isn't a killed plugin.
- The `prlimit` approach doesn't cover RSS only — `RLIMIT_AS` caps virtual memory, which overcounts for plugins that `mmap` large file ranges (e.g., tree-sitter's incremental parse buffers). Mitigation: default cap of 2 GiB is generous enough that a well-behaved plugin won't trip it; operators on constrained hosts who do trip it get a specific finding subcode.
- Full sandbox is deferred; a malicious plugin that stays under the four caps can still exfiltrate source to a network destination. This is a known v0.2 gap and is named in the "NOT in Layer 2" list and in v0.1 release notes.

### Neutral

- Enforcement point is ADR-002's subprocess supervision loop plus ADR-022's manifest-acceptance validator. No new core subsystem.
- The `capabilities.runtime` manifest block is Python-first in v0.1 but language-neutral in shape; future Java/Rust plugins inherit it verbatim.
- Configuration keys live under `loomweave.yaml:plugin_limits.*`. The v0.1 CLI documents the four keys and their floors; operators asking "can I raise this?" read one `loomweave.yaml` section.

## Related Decisions

- [ADR-002](./ADR-002-plugin-transport-json-rpc.md) — the subprocess transport is the enforcement surface for Content-Length ceiling and crash-loop counting. This ADR extends ADR-002's supervision loop with four specific violation subcodes.
- [ADR-012](./ADR-012-http-auth-default.md) — historical HTTP auth default decision. ADR-014 now owns the registry-backend HTTP read API posture; this ADR remains the plugin-side counterpart to the HTTP-side threat-model work.
- [ADR-013](./ADR-013-pre-ingest-secret-scanner.md) — pre-ingest secret scanner covers T-10 (source + secrets exfiltration); this ADR's path-jail prevents a plugin from bypassing the scanner by reading outside `project_root`, and ADR-013's scanner runs on the file list this ADR's path-jail has already filtered.
- [ADR-022](./ADR-022-core-plugin-ontology.md) — data-layer authority counterpart. This ADR's Layer 2 enforces at the transport/pipeline layer; ADR-022 enforces at the ontology layer (manifest-declared kinds, rule-ID namespacing). Both share the manifest-acceptance checkpoint.

## References

- [Panel threat-model review §7 (risk matrix)](../../implementation/v0.1-reviews/panel-2026-04-17/09-threat-model.md) — T-01, T-08, T-11, T-12 scorings.
- [Panel threat-model review §8, §12](../../implementation/v0.1-reviews/panel-2026-04-17/09-threat-model.md) — missing controls (1); three non-negotiable v0.1 controls (2).
- [Loomweave v0.1 system design §10](../v0.1/system-design.md) — threat model summary table; `Defences NOT in v0.1` (seccomp/landlock deferred).
- [Loomweave v0.1 detailed design §1.3](../v0.1/detailed-design.md) (lines 56-145) — plugin manifest shape; crash-loop circuit breaker.
- [Loomweave v0.1 scope commitments — Q3](../../implementation/v0.1-scope-plans/v0.1-scope-commitments.md) — committed decision to hybrid (declared + enforced) over full sandbox.
- [NG-16](../v0.1/requirements.md) — plugin hash-pinning deferred to v0.2; companion v0.2 control for T-01/T-15.
