# Rust Plugin Dogfood + Scale QA — Sprint 3 report (2026-06-10)

Non-normative QA memo (plan: `docs/superpowers/plans/2026-06-10-rust-plugin-scale-qa.md`,
parent ticket `clarion-a2db86704e`). Sprint-2 edge kinds (`derives`, `references`)
WERE merged before all runs (rc4 @ `48bdafd`); edge counts include them.

## Methodology

- **Installed binary**: fresh venv built exactly like `tests/e2e/rust_plugin_wheel_smoke.sh`
  (maturin release wheels for `loomweave-cli` + `packaging/rust-plugin-dist` from this
  branch, `uv pip install` into `/tmp/lw-qa-venv`), so plugin discovery runs the real
  install-prefix chain. Harness: `tests/qa/run_corpus_qa.sh` (committed).
- **Wall + peak RSS**: `getrusage(RUSAGE_CHILDREN).ru_maxrss` after wait — an
  upper bound covering the reaped host and, transitively, the plugin child (the
  heavy process). No GNU time on the box; methodology note: this is a children-max,
  not a per-process series.
- **Runs were sequential** (no CPU contention in walls). Corpora shallow-cloned,
  read-only except a benign-edit probe reverted afterwards.
- **SEI churn** from the `SEI mint pass complete` tracing line. Zero churn =
  `minted=0 AND orphaned=0` (carried < total is expected: incremental analyze
  skips unchanged files entirely, so only re-dispatched files pass through the
  mint pass).
- **Qualname collisions** via the new `qualname_check` example binary
  (`cargo run -p loomweave-plugin-rust --example qualname_check -- <dir>`).

## Corpus results (pinned commits)

| corpus | commit | wall first | peak RSS | entities | edges | run | unchanged re-analyze churn | collisions |
|---|---|---|---|---|---|---|---|---|
| ripgrep | `82313cf9` | 1.7 s | 30 MiB | 2 143 | 2 488 | completed | minted=0 orphaned=0 ✓ | **8** |
| serde | `5f0f18b9` | 1.2 s | 26 MiB | 1 624 | 1 702 | completed | minted=0 orphaned=0 ✓ | **4** |
| tokio | `2e7930fe` | 11.1 s | 43 MiB | 7 935 | 8 008 | completed¹ | minted=0 orphaned=0 ✓ | **15** |
| rust-analyzer | `587ce15e` | 141.9 s | 121 MiB | 30 369 | 41 091 | completed² | minted=0 orphaned=0 ✓ | **15** |
| Loomweave (dogfood) | working tree | 18.2 s³ | (pyright-dominated) | 5 774 (3 520 rust + 1 931 py) | 8 902 | completed | n/a (live tree) | 0 |

¹ tokio FAILED before the in-sprint fix `5dcdc3b` (see Defects D-1).
² rust-analyzer FAILED before the cap retune `f196fc1` (see Cap decisions).
³ incremental over the pre-existing index; includes the Python plugin (pyright).

Benign-edit probes (one fn body): minted=0, orphaned=0 on every corpus — identity
churn fully confined (locator-unchanged ⇒ carried).

Unresolved-call honesty (first runs): the `calls` MVP resolves a small fraction
on foreign code (e.g. tokio: 50 resolved calls edges; rust-analyzer: stats in
`/tmp` harvests) — consistent with the documented MVP envelope (bare same-module
calls and method calls land as unresolved sites, never fabricated edges).

## Cap / timeout decisions (each one line)

- **`expected_max_rss_mb` 128 → 512** (`f196fc1`, ADR-050 Amendment 1): RLIMIT_AS
  caps address space, not RSS — at 128 MiB the plugin SIGABRTs on rust-analyzer
  (~29 k entities, ~119 MiB peak RSS); completes at 1024; 512 ≈ 4× headroom.
- **Handshake 300 s / per-file 120 s / shutdown 10 s: UNCHANGED** — largest
  measured whole-repo init parse (rust-analyzer) finishes well inside 300 s;
  no per-file parse approached 120 s.
- **Parse-guard caps (bracket-depth 128, prefix-run 1024, 10 MiB file): UNCHANGED,
  measured FP rate ZERO** — no DEPTH-LIMIT or FILE-TOO-LARGE finding on any corpus.

## Guard false-positive verdict

- Depth/prefix/size guards: **0 trips / 0 FPs** across ~2 100 corpus files + Loomweave.
- `LMWV-RUST-SYNTAX-ERROR`: **100 % false positives on these corpora** — all 38
  (ripgrep 3, serde 2, tokio 18, rust-analyzer 15) are one bug: the reserved-`:`
  qualname check rejects multi-segment paths in concrete generic args
  (`Writer<$0,tt::iter::TtIter<'a>>`), and the whole file is dropped, mislabeled
  as a syntax error. Filed `clarion-8245039f6b` (implementation of the already-
  accepted ADR-049 Amendment 4; coordinate with in-flight sibling work). Dogfood
  datapoint: the only Loomweave file affected is `loomweave-core/src/plugin/host.rs`
  — `PluginHost` is absent from our own graph.

## Qualname collisions (the headline defect class)

All real-corpus collisions are ONE dialect gap: **cfg-gated twin methods inside a
single impl block** — `@cfg` discriminants are applied to module-level items and
impl blocks, never to `ImplItem::Fn` (`extract.rs:955`). Fix is additive
(only already-colliding ids change; signature discriminants rejected — cfg twins
have byte-identical signatures), but ADR-049 is frozen: requires a 4th-amendment-
style change + Wardline corpus rows in lockstep. Filed `clarion-dfeb905f46` with
the full root-cause table. Severity while unfixed: writer last-write-wins makes a
chimera entity (surviving span = last variant in source order), edges union under
one id, SEI/Wardline taint key 2–3 functions as one, and analyze emits **no
warning** (`duplicate_ids()` is only consulted by the dogfood test and
`qualname_check`). Related same-family gap: unnamed `const _` items collide
(`rust:const:span._` ×2 in rust-analyzer).

## MCP dogfood checklist (real Loomweave index, QA-venv serve)

PASS: entity_find by name (`PluginWatchdog`) · entity_find by full ADR-049
qualname (`loomweave_plugin_rust.parse_guard.scan_source`) · entity_callers_list
on that fn (resolved callers returned) · entity_neighborhood_get (HostFinding;
references_in populated) · entity_at (`parse_guard.rs:100` → enclosing rust
entity) · entity_dead_list (answers, `unresolved_call_site_suppressed` field
present) · entity_subsystem_get on a rust module · project_status_get
(completed run, rust+python counts) · index_diff_get (verdict present). 9/9.
Committed regression surface: `tests/e2e/dogfood_mcp_rust.sh` (12 checks against
a self-built fixture index; 1 pinned KNOWN-GAP: `entity_resolve` is python-only,
filed `clarion-69db8b2739`).

## Error-path UX (before/after)

- **Handshake refusal** (plugin alive, protocol error): BEFORE — spurious
  `LMWV-INFRA-PLUGIN-OOM-KILLED`; AFTER (`f525a66`, clarion-371efa3e07 closed) —
  no OOM finding; run failed with `failure_reason` carrying the plugin's own
  refusal message; a genuine mid-handshake death still classifies honestly
  (try_wait-before-kill).
- **Symlinked AGENTS.md/CLAUDE.md** (rust-analyzer ships one): BEFORE — entire
  install aborts exit 1; AFTER (`f57db4f`) — warning naming the file + remedy,
  install completes, doctor `--fix` reports skipped symlinks as actionable.
- **OOM kill mid-run**: classified (`LMWV-INFRA-PLUGIN-ABORTED` + PLUGIN-CRASH
  findings, ERROR severity) but the CLI `failure_reason` is transport-level
  ("EOF in header section") — adequate, could hint at `expected_max_rss_mb`;
  left as-is (yaml limits surface is clarion-271287b54b).
- **Secret scanner on foreign corpora**: expected behavior confirmed — tokio 85,
  rust-analyzer 82 `LMWV-SEC-SECRET-DETECTED` (fixture keys/test certs), ERROR
  severity, anchored; noisy but honest for inspect-only use.
- **Degraded-file double-report**: every degraded Rust file gets BOTH
  `LMWV-RUST-SYNTAX-ERROR` and a host-side legacy `LMWV-PY-SYNTAX-ERROR` —
  wrong-language rule id + duplicate noise; filed `clarion-a65cb18b02`.
- **`loomweave install` scaffolds agent surfaces** (skills, hooks, CLAUDE.md
  injection) into ANY project — heavyweight for inspect-only corpora; noted for
  Sprint 4 operator-docs / an install profile discussion (no ticket — design
  question, not defect).

## Defects fixed in-sprint

- **D-1 `5dcdc3b`** — analyze FAILED on tokio: dual-declared module
  (`#[path]`-mounted dir module + inline facade) made the host emit two
  `contains` parents per file_scope id → `LMWV-INFRA-PARENT-CONTAINS-MISMATCH` →
  FailRun. Host-side first-claim-wins + incremental seeding; zero entity-id
  movement (clarion-6ec7317628 closed). Residual pre-existing staleness class
  filed `clarion-feab311907`.
- **D-2 `f525a66`** — spurious OOM on handshake refusal (clarion-371efa3e07).
- **D-3 `f57db4f`** — symlinked instructions target aborted install.
- **Harness** (`0640120`..`ca3a2a4`) — committed QA harness + getrusage
  methodology.

## Subsystem clustering instability (filed)

Unchanged re-analyze changes subsystem counts (ripgrep 9→14, tokio 28→42,
rust-analyzer 103→193); non-subsystem entity sets identical, SEI churn zero.
Filed `clarion-14398b2536`.

## Reproduction

```
bash tests/qa/run_corpus_qa.sh <venv-bin> <corpus-dir> <out-dir> \
     target/release/examples/qualname_check
```
Corpora: shallow clones at the pinned commits above.
