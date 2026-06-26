# PRD-0001 — Extract `loomweave-llm` crate from `loomweave-core`     Status: SHIPPED (2026-06-26)
Decision: PDR-0003 (selection) → PDR-0005 (accepted; all 6 criteria met; PR #76 → main b346328)
Bet (roadmap.md): Now (promoted from Next this session)   Target metric (metrics.md): Trust-surface — does `loomweave-core` (plugin-host/SEI crate) link outbound HTTP (NEW)

## Problem

**Who:** Loomweave's maintainers and anyone reasoning about its trust posture —
the property the product sells is a *trustworthy* local-first graph (stable SEI,
sandboxed plugin host, credential-free `analyze`).

**The pain:** `loomweave-core` is two crates wearing one coat. It is the
**plugin-supervisor** crate — it forks each language plugin as a sandboxed
subprocess (`plugin/host.rs`, `jail.rs`, `limits.rs`, `breaker.rs`, the only
permitted `unsafe`) and it owns **stable entity identity** (`entity_id.rs`,
SEI). It *also* carries ~3,660 LOC of **outbound LLM/embedding HTTP**
(`llm_provider.rs`, `embedding_provider.rs`), and pulls `reqwest` into the
dependency tree for them. The crate that runs untrusted forked children and
mints identity tokens should not also be the crate that opens network sockets to
a model provider — every dependent of `loomweave-core` (storage, mcp, cli, the
plugin crates) transitively links that HTTP stack whether or not it ever calls a
model.

**Desired outcome:** Outbound model HTTP lives in one dedicated crate
(`loomweave-llm`); `loomweave-core` no longer links `reqwest`; the
plugin-host / SEI crate is back to a single, defensible job. Consumers that
genuinely need a provider depend on `loomweave-llm` directly.

**Why now:** This is the **head of the tracker's critical path**
(clarion-141e9c08c8). Nothing downstream — most directly the per-provider split
(clarion-4328c5c757) — can proceed cleanly until the provider code has its own
crate. The 1.1.0/gold bet is done and shipped (now at 1.3.1); this is the
next-largest unpaid structural debt and it is load-bearing for the trust story.

## Success metric (the signal the bet paid off)

**Trust-surface — does the plugin-supervisor + SEI crate (`loomweave-core`) link
an outbound HTTP client (`reqwest`)?** This metric does not yet exist on the
`metrics.md` scoreboard; adding it (BASELINE observed today) is a precondition of
ACCEPT, not a fabrication.

- `BASELINE (2026-06-24): yes` — `loomweave-core` links `reqwest` directly (verified in its Cargo.toml).
- `TARGET: no` — `loomweave-core`'s dependency tree resolves no `reqwest`, verified on the merge commit.

> **Scope of the invariant (corrected by trace, 2026-06-24):** the goal is *not*
> "centralize all HTTP in one crate." `reqwest` is legitimately used by
> `loomweave-federation` (sibling HTTP) and `loomweave-cli` (sarif/sei-git/doctor
> HTTP), and those crates neither fork sandboxed plugins nor mint SEI. The
> trust-surface invariant is specifically that the crate which *does* fork
> untrusted children and own identity must not also carry an HTTP client.

Falsification: if `cargo tree -p loomweave-core` still resolves `reqwest` after
the bet lands, the bet did not pay off, regardless of how much code moved.

## Acceptance criteria (falsifiable)

> Observation window for every criterion below is **the CI run on the merge
> commit of this bet** — a concrete event, not "eventually." The *calendar*
> forecast for when that merge happens is `/axiom-program-management`'s, not this
> PRD's.

1. **SUCCESS (structural)** — On the merge commit, `cargo tree -p loomweave-core`
   resolves **no `reqwest`** dependency, and both provider modules
   (`llm_provider`, `embedding_provider`) with their traits and impls live in a
   new `loomweave-llm` crate.
   *Reject branch:* `reqwest` still in `loomweave-core`'s tree → bet **not
   accepted**; the trust boundary was not achieved; open a follow-up PDR.

2. **METRIC (trust-surface, from amended `metrics.md`)** — Trust-surface reading
   flips **yes → no** on the merge commit: `loomweave-core` no longer links
   `reqwest`. Enforced by a **CI assertion** that `cargo tree -p loomweave-core
   --edges normal` resolves no `reqwest` (a per-dependent ban that `cargo-deny`'s
   `[bans]` cannot express — `reqwest` stays legitimate for federation/cli, so it
   is *not* denied workspace-wide).
   *Reject branch:* `loomweave-core` still resolves `reqwest` → bet **rejected
   even if (1) reads done**.

3. **GUARDRAIL — CI floor green (`metrics.md`, ADR-023)** — every floor gate
   passes on the merge commit: `fmt`, `clippy --all-targets --all-features
   -D warnings`, `build`, `nextest`, `doc -D warnings`, `deny`, plus `ruff`,
   `ruff format --check`, `mypy --strict`, `pytest`, and the three e2e scripts.
   *Reject branch:* any gate red → bet **rejected even if (1)+(2) pass**.

4. **GUARDRAIL — identity stability (`metrics.md`, SEI-churn-0)** — re-analyze of
   the reference corpora shows **0 SEI churn** vs. pre-bet, and `entity_id.rs` is
   **not modified** by the extraction (the LLM crate must not pull in or alter
   entity-ID code).
   *Reject branch:* any SEI churn, or `entity_id.rs` touched by the move → bet
   **rejected** (identity is the product's core promise).

5. **GUARDRAIL — no consumer regression** — every current provider consumer
   (`loomweave-cli` analyze/config/doctor/serve, `loomweave-mcp`
   semantic/summary, `loomweave-federation/config`) compiles and its tests pass
   against the new boundary; provider-replay tests (`RecordingProvider`,
   `RecordingEmbeddingProvider`) pass **unchanged**.
   *Reject branch:* any consumer behavior change, or a test that had to be
   loosened to pass → bet **rejected**; it stopped being a lift-and-shift.

6. **SCOPE — pure lift-and-shift** — no provider *behavior* changes: no new
   providers, no transport/retry/caching/timeout logic changes, no config-schema
   changes. The diff is relocation + re-wiring only.
   *Reject branch:* any provider behavior change → out of scope for this bet;
   carve it into a separate bet.

## Non-goals (this bet)

- **Not** the per-provider split of `llm_provider.rs` (OpenRouter / Codex-CLI
  into separate modules) — that is the downstream bet **clarion-4328c5c757**,
  unblocked *by* this one. This bet only relocates the existing module wholesale.
- **No** new provider behavior, retry/caching/transport/timeout changes, or
  `llm_policy` config-schema changes.
- **No** changes to `entity_id`/SEI, the plugin host (`plugin/`), `storage`, or
  the MCP/HTTP read surface.
- **No** change to summary/embedding *semantics* (lazy, per-entity, opt-in;
  `analyze` stays credential-free).

## Constraints & guardrails

- **Dependency direction is the load-bearing boundary:** `loomweave-core` must
  **not** gain a dependency on `loomweave-llm` (that would re-introduce `reqwest`
  transitively and void the whole bet). A compat re-export living *in*
  `loomweave-core` is therefore **ruled out**. *Trace finding (2026-06-24):* the
  two provider modules import **no** workspace code (`std` + `async-trait` /
  `fs2` / `serde` / `thiserror` / `reqwest` / `tokio` only), so `loomweave-llm`
  is a **pure leaf crate** — the direction is trivially acyclic. Consumers (`cli`,
  `mcp`, and any federation use) repoint provider imports `loomweave_core::…` →
  `loomweave_llm::…`. *The exact mechanism (shared error crate? type placement)
  is `/axiom-solution-architect`'s to ratify — but the trace says none is needed.*
- **Version lockstep:** the new crate uses `version.workspace = true`; all
  `scripts/check-*.py` lockstep guards (workspace version, entity cap, pyright
  pin, ontology version, migration retirement) stay green.
- **Workspace hygiene:** `unsafe_code = "deny"` (the move introduces no unsafe);
  clippy `pedantic -D warnings` workspace-wide; edition 2024, `rust-version 1.88`.
- **Anti-goals preserved (`vision.md`):** local-first, no eager LLM spend, no
  mandatory cloud — unchanged by relocation.

## Open questions / assumptions

- **ASSUMPTION (provenance):** the decision to make this the Now bet (PDR-0003)
  was confirmed by the owner this session but is **not yet written** to
  `decisions/` — it lands at `/product-checkpoint`. This PRD's authority rests on
  that in-session DECIDE.
- **ASSUMPTION (metric gate):** the trust-surface guardrail is **added to
  `metrics.md`** (BASELINE `loomweave-core links reqwest: yes` → TARGET `no`) as
  part of accepting this bet. Until it is, criterion 2 references a metric not yet
  on the scoreboard — that amendment is an ACCEPT precondition (also a checkpoint
  action). The BASELINE is observed, not invented.
- **CORRECTION (metric scope, 2026-06-24):** the metric was initially drafted as
  "no crate outside `loomweave-llm` links `reqwest`" and **corrected by trace** —
  `reqwest` is legitimately used by `loomweave-federation` and `loomweave-cli`.
  The invariant is `loomweave-core`-specific. The bet is unchanged; only the
  measurement was made achievable.
- **RESOLVED (consumer set, ratified by solution-architect 2026-06-24):** the
  re-wire is **`loomweave-cli`** (`src/serve.rs`, `src/analyze.rs`,
  `tests/serve.rs`) and **`loomweave-mcp`** (`src/lib.rs`, `src/tools/summary.rs`,
  `src/tools/status.rs`, `tests/storage_tools.rs`, `tests/catalogue_tools.rs`) —
  both already link their own `reqwest`, so no new HTTP surface. **`loomweave-
  federation` and `loomweave-storage` are confirmed NON-consumers** (federation
  defines its own config enums; it never constructs a provider). Planning input,
  not an acceptance gate.
- **ASSUMPTION (cohesion):** `CodexCliProvider` (CLI-based, no `reqwest`) moves
  with the LLM module for cohesion, so the whole `LlmProvider` abstraction lives
  in one crate.

## Handoff

- **Top item → `/axiom-planning`:** the crate extraction + consumer re-wire as a
  **behavior-preserving move** (tracker **clarion-141e9c08c8**) — the executable,
  codebase-validated plan for the lift-and-shift and the `cargo-deny` ban rule.
- **Solution shape → `/axiom-solution-architect`:** the crate boundary and
  dependency direction (compat re-export vs. direct consumer imports; where the
  shared error/types live; cycle-avoidance), and the design of the
  ban-`reqwest`-outside-`loomweave-llm` check. The PRD names the constraints; the
  design chooses within them.
- **Sequencing & dated forecast → `/axiom-program-management`** (no date here).
- **Tracker linkage:** clarion-141e9c08c8 (this bet) → unblocks
  clarion-4328c5c757 (per-provider split, the next bet).
