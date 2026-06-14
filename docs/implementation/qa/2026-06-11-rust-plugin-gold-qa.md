# Rust Plugin Gold Closeout — Sprint 4 QA report (2026-06-11)

Non-normative QA memo. Branch `feat/rust-plugin-gold` off rc4. Parent ticket
`clarion-363a6ca7d3`. Harness: `tests/qa/run_corpus_qa.sh` (unchanged from
Sprint-3), re-run with a wheel built from this branch. Collision oracle:
`cargo run -p loomweave-plugin-rust --example qualname_check`. Corpora re-cloned
at the Sprint-3 pinned SHAs.

## Headline

Two ticketed identity defects are **fully closed**; two more gold criteria are
met; **but the zero-collision gold bar is not met.** Re-running the Sprint-3
harness with a per-id enumeration revealed that **Sprint-3 mis-attributed its
collision count** — what it reported as "all cfg-twin methods" is in fact four
distinct dialect families. The cfg-twin-method family is now provably eliminated;
the other three (plus a const family Sprint-3 under-counted) remain and are filed.

## What was fixed (with before/after)

| defect | ticket | before | after | status |
|---|---|---|---|---|
| cfg-twin methods collide | `clarion-dfeb905f46` | ripgrep 8 / serde 4 collisions of this family | **0** (ripgrep, serde); completeness-verified zero cfg-twin-method residuals across tokio + rust-analyzer | **CLOSED** (ADR-049 Amendment 5) |
| reserved-`:` whole-file drops | `clarion-8245039f6b` | 3 / 2 / 18 / 15 = **38** files dropped | **0 / 0 / 0 / 0** | **CLOSED** (ADR-049 Amendment 4 impl + self-type-fallback completion) |

`PluginHost` is back in Loomweave's own graph: a fresh analyze of
`loomweave-core` with the fixed wheel mints
`rust:function:loomweave_core.plugin.host.PluginHost<$0,$1>.impl#<$0,$1>.*`
(81 entities in `plugin/host.rs`, 0 degraded) — it was entirely absent before.

## Per-corpus final numbers (pinned SHAs)

| corpus | commit | entities | collisions (was S3) | reserved-`:` drops (was S3) | SEI churn (unchanged re-analyze) |
|---|---|---|---|---|---|
| ripgrep | `82313cf9` | 2 169 | **0** (was 8) | **0** (was 3) | minted=0 orphaned=0 ✓ |
| serde | `5f0f18b9` | 1 673 | **0** (was 4) | **0** (was 2) | minted=0 orphaned=0 ✓ |
| tokio | `2e7930fe` | 7 771 | **12** (was 15) | **0** (was 18) | minted=0 orphaned=0 ✓ |
| rust-analyzer | `587ce15e` | 31 376 | **15** (was 15) | **0** (was 15) | minted=0 orphaned=0 ✓ |

Entity counts rose vs Sprint-3 (e.g. serde 1 624 → 1 673, ripgrep 2 143 → 2 169)
because the previously-dropped reserved-`:` files now ingest. The one residual
`LMWV-RUST-SYNTAX-ERROR` on rust-analyzer is a **legitimate** degrade
(`minicore.rs` uses a `//- minicore:` pseudo-syntax that is genuinely
unparseable), not a reserved-`:` defect.

## Correction to the Sprint-3 report

`docs/implementation/qa/2026-06-10-rust-plugin-scale-qa.md` states "All real-corpus
collisions are ONE dialect gap: cfg-gated twin methods inside a single impl block."
**This is incorrect.** A per-id enumeration of the residuals (with the cfg-twin
family now fixed) shows the 27 remaining collisions are four families, none of
which is cfg-twin-methods:

| family | corpus evidence | count | root cause | ticket |
|---|---|---|---|---|
| **self-type last-segment** | tokio `Semaphore.impl[Semaphore].*` (`bounded::Semaphore` vs `unbounded::Semaphore`) | 5 | `self_ty_locator` keeps only the last path segment of the self type | `clarion-8ff7f233fa` |
| **trait-path last-segment** | tokio_util `Compat<$0>.impl[AsyncRead/AsyncWrite/AsyncBufRead].*` (`tokio::io::AsyncRead` vs `futures_io::AsyncRead`) | 5 | `impl_disc_for` keys the trait on its last segment only | `clarion-fa8bcf8731` |
| **`#[path]` module alias** | tokio `process.unix` / `process.windows` (×2) | 2 | file-walk module-path builder is `#[path]`/cfg-blind, aliases an inline `mod` of the dir's own name | `clarion-bdb1eccf48` |
| **unnamed `const _`** | rust-analyzer `intern.symbol._`, `tt.storage._`, … (incl. one cfg-suffixed) | 15 | `const _` all render `{module}._`; cfg cannot disambiguate an anonymous name | `clarion-83870dc534` |

Each was confirmed with a minimal reproduction against the `qualname_check`
oracle. All four change emitted qualnames (or the emitted entity set, for
`const _`), so each requires its own ADR-049 amendment **and** a Wardline-lockstep
re-vendor. Because Wardline's Rust frontend has graduated to its `rc5` release
branch, introducing three-plus new dialect amendments at it is escalation-gated —
out of a gold-closeout's scope. They are filed as gold blockers for the user to
schedule.

## Cross-repo (Wardline) state

Amendments 4 (reserved-`:` + const spacing, incl. the self-type-fallback
completion) and 5 (cfg-twin methods) are implemented Loomweave-side with 7 new
corpus rows; the handoff change-set is
`docs/federation/2026-06-11-rust-qualname-amendment-4-5-changeset.md` (corpus
35 rows, md5 `bf8d09968b5d366a8bd033710d736744`). **Not pushed** at Wardline's
`rc5` — handoff only; the Loomweave owner schedules the re-vendor.

## Gold verdict

**Not gold.** Single named reason: **real-corpus qualname collisions are not
zero** — 27 collisions across four dialect families (self-type-path, trait-path,
`#[path]`-module, `const _`) remain, each a distinct silent-data-loss gap that
Sprint-3 folded into its cfg-twin-method count. The cfg-twin-method defect and the
reserved-`:` drop defect — the two that were *ticketed* as the gold blockers — are
both fully closed, and SEI stability, conformance, and the full CI floor are green.
Reaching gold requires closing the four filed families, each via an ADR-049
amendment coordinated with (now-graduated) Wardline.

## Reproduction

```
bash tests/qa/run_corpus_qa.sh <venv-bin> <corpus-dir> <out-dir> \
     target/release/examples/qualname_check
# venv = fresh uv venv with this branch's two maturin wheels (--no-deps),
# per tests/e2e/rust_plugin_wheel_smoke.sh.
```
