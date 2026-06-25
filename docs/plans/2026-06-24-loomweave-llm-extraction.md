# `loomweave-llm` Crate Extraction — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Move the LLM + embedding provider code out of `loomweave-core` into a new pure-leaf crate `loomweave-llm`, so the plugin-supervisor + SEI crate (`loomweave-core`) no longer links an outbound HTTP client (`reqwest`).

**Architecture:** Behavior-preserving lift-and-shift. The two provider modules (`llm_provider.rs`, `embedding_provider.rs`) move verbatim — including their `#[cfg(test)]` modules — into a new leaf crate that depends on **no** workspace crate. `loomweave-core` drops `reqwest`/`async-trait`/`fs2`. The two current consumers (`loomweave-cli`, `loomweave-mcp`) repoint their provider imports from `loomweave_core::` to `loomweave_llm::`. **No provider behavior changes; no per-provider split** (that is the downstream bet clarion-4328c5c757, explicitly out of scope).

**Tech Stack:** Rust workspace (resolver 3, edition 2024, rust-version 1.88), `cargo nextest`, `cargo-deny`, clippy pedantic `-D warnings`, `unsafe_code = "deny"`. Source PRD: `docs/product/prd/PRD-0001-loomweave-llm-extraction.md`. Tracker: clarion-141e9c08c8.

**Prerequisites:**
- Clean working tree on a feature branch (e.g. `feat/loomweave-llm-extraction`). Do **not** work on `main`.
- Toolchain already pinned via `rust-toolchain`; the Python venv at `plugins/python/.venv` exists (Python gates are unaffected by this change but run in the floor).
- Read the source PRD and this plan's **Ground truth** section before starting.

---

## Ground truth (verified against the codebase 2026-06-24 — trust these, do not re-derive)

**Files to move** (wholesale, with their test modules):
- `crates/loomweave-core/src/llm_provider.rs` (3198 LOC)
- `crates/loomweave-core/src/embedding_provider.rs` (460 LOC)

**`reqwest` lives only in those two files** within `loomweave-core/src` (4 occurrences total). Nothing else in core links HTTP.

**Deps after the move:**
- `loomweave-core/Cargo.toml` — REMOVE `async-trait`, `fs2`, `reqwest` (verified now-unused by remaining core code). KEEP `tempfile`, `tracing`, `serde_json`, `which`, `tokio`, `serde`, `thiserror`, `toml`, `nix`.
- New `loomweave-llm/Cargo.toml` — needs `async-trait`, `fs2`, `reqwest`, `serde`, `serde_json`, `tempfile`, `thiserror`, `tokio`, `tracing`, `which` (all already in `[workspace.dependencies]`).

**The provider modules import zero workspace code** — only `std`, `async-trait`, `fs2`, `reqwest`, `serde`, `serde_json`, `tempfile`, `thiserror`, `tokio`, `tracing`, `which`. `embedding_provider.rs` references `crate::llm_provider` **only in doc-comments**, which stay valid because both modules move together. ⇒ `loomweave-llm` is a pure leaf; no `loomweave-core → loomweave-llm` edge, no cycle.

**The complete set of provider symbols that move** (the current `pub use` lists in `loomweave-core/src/lib.rs` lines 17–31):
- from `embedding_provider`: `ApiEmbeddingProvider, ApiEmbeddingProviderConfig, EmbeddingProvider, EmbeddingProviderError, EmbeddingRecording, RecordingEmbeddingProvider`
- from `llm_provider`: `CachingModel, ClaudeCliProvider, ClaudeCliProviderConfig, CodexCliProvider, CodexCliProviderConfig, INFERRED_CALLS_PROMPT_VERSION, InferredCallsPromptInput, LEAF_SUMMARY_PROMPT_TEMPLATE_ID, LeafSummaryPromptInput, LlmProvider, LlmProviderError, LlmPurpose, LlmRequest, LlmResponse, OpenRouterProvider, OpenRouterProviderConfig, PromptTemplate, Recording, RecordingProvider, TrafficLoggingProvider, build_coding_agent_provider_prompt, build_inferred_calls_prompt, build_leaf_summary_prompt`

**Symbols that STAY in `loomweave-core`** (do not touch their imports): `McpErrorCode`, `EdgeConfidence`, `HttpErrorCode`, `EntityId`, everything under `loomweave_core::{plugin, store, entity_id, errors, hardened_git}`.

**The complete consumer import-site set** (8 edit sites across 2 crates — verified exhaustive via per-file reads + a global backstop grep):
- `loomweave-cli/src/serve.rs:9–14` · `loomweave-cli/src/analyze.rs:26–30` and `:7984` and `:8053`
- `loomweave-mcp/src/lib.rs:18–21` and `:6099` · `loomweave-mcp/src/tools/summary.rs:11–15` · `loomweave-mcp/src/tools/status.rs:9`
- `loomweave-cli/tests/serve.rs:13–16` · `loomweave-mcp/tests/storage_tools.rs:11–16` · `loomweave-mcp/tests/catalogue_tools.rs:9`

**Confirmed NON-consumers (do NOT edit):** `loomweave-federation`, `loomweave-storage`, `loomweave-cli/src/config.rs`, `loomweave-cli/src/doctor.rs`, `loomweave-mcp/src/catalogue/semantic.rs`. The first two never construct a provider; the last three use `loomweave_federation::config::{LlmProviderKind, ProviderSelection, …}` (federation config enums) and/or concrete in-crate types — **not** the moved traits.

**Path-dep style in this workspace:** `loomweave-llm = { path = "../loomweave-llm", version = "1.3.1" }`.

**CI floor lives in** `.github/workflows/verify.yml`. **`scripts/check-workspace-version-lockstep.py` needs NO change** (it tracks `pyproject.toml` files, not per-crate `Cargo.toml`; `version.workspace = true` satisfies lockstep).

---

## Task 1: Scaffold `loomweave-llm` and copy the provider modules in (workspace stays green)

This task **copies** the modules so both crates compile at the commit boundary. Task 2 removes core's copies and flips consumers. Copy-then-flip keeps every commit green.

**Files:**
- Create: `crates/loomweave-llm/Cargo.toml`
- Create: `crates/loomweave-llm/src/lib.rs`
- Create: `crates/loomweave-llm/src/llm_provider.rs` (copy of core's)
- Create: `crates/loomweave-llm/src/embedding_provider.rs` (copy of core's)
- Modify: `Cargo.toml` (root — workspace `members`)

**Step 1: Create the crate's `Cargo.toml`**

```toml
[package]
name = "loomweave-llm"
description = "Loomweave LLM + embedding provider traits, concrete providers (OpenRouter / Codex CLI / Claude CLI), and the outbound HTTP/CLI transport for summaries and embeddings."
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
async-trait.workspace = true
fs2.workspace = true
reqwest.workspace = true
serde.workspace = true
serde_json.workspace = true
tempfile.workspace = true
thiserror.workspace = true
tokio.workspace = true
tracing.workspace = true
which.workspace = true
```

**Why no `[dev-dependencies]`:** the moved test modules use only `std` + crates already in `[dependencies]` (`tempfile`, `tokio`'s test macros via the workspace feature set). If `cargo nextest` later reports a missing test-only crate, add it then — but it is not expected.

**Step 2: Copy the two module files into the new crate**

```bash
cp crates/loomweave-core/src/llm_provider.rs crates/loomweave-llm/src/llm_provider.rs
cp crates/loomweave-core/src/embedding_provider.rs crates/loomweave-llm/src/embedding_provider.rs
```

Do **not** edit their contents — they are self-contained (no `crate::` code references; doc-links resolve within the new crate).

**Step 3: Create `crates/loomweave-llm/src/lib.rs`** (mirrors core's exact re-export lists)

```rust
//! loomweave-llm — LLM + embedding provider traits, concrete providers, and the
//! outbound HTTP/CLI transport for Loomweave summaries and embeddings.
//!
//! Extracted from `loomweave-core` (PRD-0001, clarion-141e9c08c8) so the
//! plugin-supervisor + SEI crate does not link an outbound HTTP client.

pub mod embedding_provider;
pub mod llm_provider;

pub use embedding_provider::{
    ApiEmbeddingProvider, ApiEmbeddingProviderConfig, EmbeddingProvider, EmbeddingProviderError,
    EmbeddingRecording, RecordingEmbeddingProvider,
};
pub use llm_provider::{
    CachingModel, ClaudeCliProvider, ClaudeCliProviderConfig, CodexCliProvider,
    CodexCliProviderConfig, INFERRED_CALLS_PROMPT_VERSION, InferredCallsPromptInput,
    LEAF_SUMMARY_PROMPT_TEMPLATE_ID, LeafSummaryPromptInput, LlmProvider, LlmProviderError,
    LlmPurpose, LlmRequest, LlmResponse, OpenRouterProvider, OpenRouterProviderConfig,
    PromptTemplate, Recording, RecordingProvider, TrafficLoggingProvider,
    build_coding_agent_provider_prompt, build_inferred_calls_prompt, build_leaf_summary_prompt,
};
```

**Step 4: Register the crate in the workspace** — add `"crates/loomweave-llm",` to the `members` array in the root `Cargo.toml` (keep it grouped with the other crates, e.g. right after `"crates/loomweave-core",`).

**Step 5: Verify the new crate compiles, lints, and its tests pass in isolation**

Run:
```bash
cargo build -p loomweave-llm
cargo clippy -p loomweave-llm --all-targets --all-features -- -D warnings
cargo nextest run -p loomweave-llm
cargo build --workspace   # core still has its own copies → whole workspace still green
```

Expected: all green. The workspace builds because `loomweave-core` is unchanged (it still owns its copies) and `loomweave-llm` is a new, not-yet-consumed leaf.

**Step 6: Commit**

```bash
git add crates/loomweave-llm Cargo.toml
git commit -m "feat(loomweave-llm): scaffold crate with copied provider modules

Pure-leaf crate holding the LLM + embedding providers, copied from
loomweave-core. Consumers are flipped and core's copies removed in the
next commit (PRD-0001, clarion-141e9c08c8).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

**Definition of Done:**
- [ ] `loomweave-llm` builds, clippies clean, and its moved tests pass in isolation.
- [ ] `cargo build --workspace` still green (core unchanged).
- [ ] Committed.

---

## Task 2: Flip core + consumers to `loomweave-llm`; remove the providers from core (green → green)

This is the load-bearing transition. Its sub-steps are **not** independently compilable — the workspace goes red between Step 1 and the last edit, then green again. **Commit only at the end, when the floor passes.**

**Files:**
- Delete: `crates/loomweave-core/src/llm_provider.rs`, `crates/loomweave-core/src/embedding_provider.rs`
- Modify: `crates/loomweave-core/src/lib.rs`, `crates/loomweave-core/Cargo.toml`
- Modify: `crates/loomweave-cli/Cargo.toml`, `crates/loomweave-mcp/Cargo.toml`
- Modify: the 8 consumer import sites listed under Ground truth

**Step 1: Remove the modules from `loomweave-core`**

```bash
git rm crates/loomweave-core/src/llm_provider.rs crates/loomweave-core/src/embedding_provider.rs
```

**Step 2: Strip the provider declarations + re-exports from `crates/loomweave-core/src/lib.rs`**

- Delete line 9 `pub mod embedding_provider;` and line 13 `pub mod llm_provider;`.
- Delete the `pub use embedding_provider::{ … };` block (lines 17–20) and the `pub use llm_provider::{ … };` block (lines 24–31).
- **Keep** `pub use entity_id::{…}`, `pub use errors::{…}`, `pub use hardened_git::{…}`, and the whole `pub use plugin::{…}` block.

After editing, the top of `lib.rs` reads (module list):
```rust
pub mod entity_id;
pub mod errors;
pub mod hardened_git;
pub mod plugin;
pub mod store;
```

**Step 3: Drop the now-unused deps from `crates/loomweave-core/Cargo.toml`**

Remove these three lines from `[dependencies]`:
```toml
async-trait.workspace = true
fs2.workspace = true
reqwest.workspace = true
```
Update the `description` to drop the provider mention, e.g.:
```toml
description = "Loomweave core: entity-ID assembler, sandboxed JSON-RPC plugin host, and manifest parser."
```
(Leave `[dev-dependencies] tempfile` as-is.)

**Step 4: Add the `loomweave-llm` dependency to the two consumer crates**

In `crates/loomweave-cli/Cargo.toml` and `crates/loomweave-mcp/Cargo.toml`, add to `[dependencies]` (next to the existing `loomweave-core` line):
```toml
loomweave-llm = { path = "../loomweave-llm", version = "1.3.1" }
```

**Step 5: Rewire the 8 import sites.** Each edit below is exact (old → new).

**5a — `loomweave-cli/src/serve.rs:9–14`** (whole block moves; change the path only):
```rust
use loomweave_llm::{
    ApiEmbeddingProvider, ApiEmbeddingProviderConfig, ClaudeCliProvider, ClaudeCliProviderConfig,
    CodexCliProvider, CodexCliProviderConfig, EmbeddingProvider, EmbeddingProviderError,
    LlmProvider, OpenRouterProvider, OpenRouterProviderConfig, Recording, RecordingProvider,
    TrafficLoggingProvider,
};
```

**5b — `loomweave-cli/src/analyze.rs:26–30`** (split out `EmbeddingProvider`):
```rust
use loomweave_core::{
    AcceptedEdge, AcceptedEntity, AnalyzeFileOutcome, CrashLoopBreaker, CrashLoopState,
    DiscoveredPlugin, FINDING_DISABLED_CRASH_LOOP, HostError, HostFinding, UnresolvedCallSite,
    discover,
};
use loomweave_llm::EmbeddingProvider;
```

**5c — `loomweave-cli/src/analyze.rs:7984` and `:8053`** (identical lines, both inside test fns — change the path; an exact-string replace-all hits both):
```rust
        use loomweave_llm::{EmbeddingProvider, EmbeddingRecording, RecordingEmbeddingProvider};
```

**5d — `loomweave-cli/tests/serve.rs:13–16`** (split `LEAF_SUMMARY_PROMPT_TEMPLATE_ID` from the kept `plugin::` path):
```rust
use loomweave_core::plugin::{ContentLengthCeiling, Frame, read_frame, write_frame};
use loomweave_llm::LEAF_SUMMARY_PROMPT_TEMPLATE_ID;
```

**5e — `loomweave-mcp/src/lib.rs:18–21`** (split — `EdgeConfidence`/`McpErrorCode` stay):
```rust
use loomweave_core::{EdgeConfidence, McpErrorCode};
use loomweave_llm::{EmbeddingProvider, LlmProvider, LlmProviderError, LlmRequest, LlmResponse};
```

**5f — `loomweave-mcp/src/lib.rs:6099`** (test module; whole set moves):
```rust
    use loomweave_llm::{CachingModel, LlmProvider, LlmProviderError, LlmRequest, LlmResponse};
```

**5g — `loomweave-mcp/src/tools/summary.rs:11–15`** (split — `EdgeConfidence`/`McpErrorCode` stay):
```rust
use loomweave_core::{EdgeConfidence, McpErrorCode};
use loomweave_llm::{
    INFERRED_CALLS_PROMPT_VERSION, InferredCallsPromptInput, LEAF_SUMMARY_PROMPT_TEMPLATE_ID,
    LeafSummaryPromptInput, LlmPurpose, LlmRequest, build_inferred_calls_prompt,
    build_leaf_summary_prompt,
};
```

**5h — `loomweave-mcp/src/tools/status.rs:9`** (split — `McpErrorCode` stays):
```rust
use loomweave_core::McpErrorCode;
use loomweave_llm::{LeafSummaryPromptInput, build_leaf_summary_prompt};
```

**5i — `loomweave-mcp/tests/storage_tools.rs:11–16`** (whole set moves; change the path only):
```rust
use loomweave_llm::{
    CachingModel, INFERRED_CALLS_PROMPT_VERSION, InferredCallsPromptInput,
    LEAF_SUMMARY_PROMPT_TEMPLATE_ID, LeafSummaryPromptInput, LlmProvider, LlmProviderError,
    LlmPurpose, LlmRequest, LlmResponse, OpenRouterProvider, OpenRouterProviderConfig, Recording,
    RecordingProvider, build_inferred_calls_prompt, build_leaf_summary_prompt,
};
```

**5j — `loomweave-mcp/tests/catalogue_tools.rs:9`** (both move):
```rust
use loomweave_llm::{EmbeddingRecording, RecordingEmbeddingProvider};
```

**Step 6: Compiler backstop — catch anything the enumerated edits missed**

Run, in order:
```bash
# (i) No moved symbol should still be referenced via loomweave_core:: anywhere.
grep -rnE 'loomweave_core::[^;]*(LlmProvider|EmbeddingProvider|OpenRouterProvider|ApiEmbeddingProvider|CodexCliProvider|ClaudeCliProvider|TrafficLoggingProvider|RecordingEmbeddingProvider|RecordingProvider|EmbeddingRecording|CachingModel|LlmRequest|LlmResponse|LlmPurpose|LeafSummaryPromptInput|InferredCallsPromptInput|build_leaf_summary_prompt|build_inferred_calls_prompt|build_coding_agent_provider_prompt|PromptTemplate|LEAF_SUMMARY_PROMPT_TEMPLATE_ID|INFERRED_CALLS_PROMPT_VERSION)' crates/*/src crates/*/tests --include='*.rs' | grep -v 'crates/loomweave-llm/' || echo "CLEAN"
# (ii) Full workspace build (the exhaustive backstop).
cargo build --workspace --all-targets
```
If (i) is not `CLEAN`, repoint each remaining site `loomweave_core::X` → `loomweave_llm::X`. **Watch item:** `loomweave-mcp/src/catalogue/semantic.rs` calls `state.provider.model_id()` (an `EmbeddingProvider` trait method) but does not import the trait from `loomweave_core`; it is expected to keep compiling unchanged. If (ii) reports the trait is not in scope there, add `use loomweave_llm::EmbeddingProvider;` to that file — the only anticipated surprise.

**Step 7: Run the full CI floor**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
plugins/python/.venv/bin/ruff check plugins/python
plugins/python/.venv/bin/ruff format --check plugins/python
plugins/python/.venv/bin/mypy --strict plugins/python
plugins/python/.venv/bin/pytest plugins/python
```
Expected: all green. Then confirm the trust-surface invariant:
```bash
cargo tree -p loomweave-core --edges normal | grep -q '^reqwest' && echo "FAIL: core still links reqwest" || echo "PASS: core has no reqwest"
```
Expected: `PASS`.

**Step 8: Commit**

```bash
git add -A
git commit -m "refactor(core): extract LLM/embedding providers into loomweave-llm

loomweave-core no longer links reqwest/async-trait/fs2. The two provider
modules now live in the pure-leaf loomweave-llm crate; cli and mcp repoint
their provider imports. Behavior-preserving lift-and-shift; no per-provider
split (clarion-4328c5c757 remains separate). PRD-0001, clarion-141e9c08c8.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

**Definition of Done:**
- [ ] `git rm` of both core modules; core `lib.rs` + `Cargo.toml` stripped of providers and `reqwest`/`async-trait`/`fs2`.
- [ ] `loomweave-llm` dep added to cli + mcp; all 8 import sites rewired; backstop grep CLEAN.
- [ ] Full floor green; `cargo tree -p loomweave-core` shows no `reqwest`.
- [ ] Committed.

---

## Task 3: Add the trust-surface CI gate

Make the invariant standing, not a one-time check.

**Files:**
- Modify: `.github/workflows/verify.yml`

**Step 1: Add a gate step to the Rust job** (place it after the build/clippy steps, before or alongside `cargo deny`):

```yaml
      - name: Trust-surface — loomweave-core must not link an HTTP client
        run: |
          if cargo tree -p loomweave-core --edges normal | grep -q '^reqwest'; then
            echo "::error::loomweave-core links reqwest; the provider HTTP must stay in loomweave-llm (PRD-0001)"
            exit 1
          fi
          echo "OK: loomweave-core has no reqwest in its dependency tree"
```

**Step 2: Verify the step's command passes locally**

```bash
if cargo tree -p loomweave-core --edges normal | grep -q '^reqwest'; then echo FAIL; exit 1; else echo OK; fi
```
Expected: `OK`.

**Step 3: Commit**

```bash
git add .github/workflows/verify.yml
git commit -m "ci(verify): assert loomweave-core links no outbound HTTP client

Standing trust-surface gate for PRD-0001: fails CI if reqwest re-enters
loomweave-core's dependency tree.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

**Definition of Done:**
- [ ] Gate step added to `verify.yml`; command verified locally.
- [ ] Committed.

---

## Task 4: Acceptance verification + close the tracker item

No code changes — verify every PRD-0001 acceptance criterion and bank the bet.

**Step 1: Re-run the full floor** (Task 2 Step 7) and confirm all green.

**Step 2: Verify each acceptance criterion explicitly:**
- **Criterion 1/2 (trust-surface):** `cargo tree -p loomweave-core --edges normal | grep reqwest` → no output.
- **Criterion 3 (CI floor):** all gates green (above).
- **Criterion 4 (identity stability):** `git diff --name-only main...HEAD | grep entity_id.rs` → **empty** (entity_id.rs untouched). SEI churn: not expected — no identity code moved. If a reference corpus is handy, a before/after `loomweave analyze` should show 0 SEI churn; otherwise the untouched-`entity_id.rs` check is the proxy the PRD allows.
- **Criterion 5 (no consumer regression):** cli + mcp tests pass (covered by `cargo nextest run --workspace`); the `RecordingProvider` / `RecordingEmbeddingProvider` replay tests now run in `loomweave-llm` and pass **unchanged** (their source was not edited).
- **Criterion 6 (pure lift-and-shift):** `git diff main...HEAD -- crates/loomweave-llm/src/llm_provider.rs crates/loomweave-llm/src/embedding_provider.rs` shows **only** the file relocation (no content delta vs. the deleted core copies). Confirm with `git log --follow` / a rename-aware diff.

**Step 3: Update the tracker**

```bash
filigree close clarion-141e9c08c8 --actor claude
# clarion-4328c5c757 (per-provider split) is now unblocked — leave it for the next bet.
```

**Step 4 (product loop):** report back so `/product-checkpoint` can bank the acceptance and add the trust-surface guardrail to `metrics.md` (BASELINE `loomweave-core links reqwest: yes` → TARGET `no`, now met).

**Definition of Done:**
- [ ] All six acceptance criteria verified with the commands above.
- [ ] `clarion-141e9c08c8` closed; `clarion-4328c5c757` noted as unblocked.
- [ ] Hand back to the product loop for checkpoint.

---

## Risks & rollback

- **Largest risk:** a missed fully-qualified `loomweave_core::<provider>` reference. Mitigated by the Task 2 Step 6 backstop grep + `cargo build --workspace --all-targets`; the compiler names any straggler exactly.
- **`semantic.rs` trait scope:** the one anticipated surprise (see Task 2 Step 6 watch item) — a one-line `use loomweave_llm::EmbeddingProvider;` fix if it surfaces.
- **Rollback:** the change is two code commits + one CI commit on a feature branch; `git revert` or branch-delete restores the prior state cleanly. No data, schema, or protocol surface is touched.

## Validate before execution (recommended)

This is a structural refactor touching a load-bearing crate boundary. **RECOMMENDED SUB-SKILL:** run `/review-plan docs/plans/2026-06-24-loomweave-llm-extraction.md` (reality / architecture / quality / systems reviewers) before executing. Proceed on `APPROVED` / `APPROVED_WITH_WARNINGS`; revise on `CHANGES_REQUESTED`.
