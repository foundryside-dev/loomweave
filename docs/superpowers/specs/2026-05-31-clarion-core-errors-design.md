# Design — `clarion-core::errors` shared error vocabulary

**Ticket:** clarion-b57c6bc49f (feature, P2, `release:v1.1`, `category:architecture`).
**Closes:** V11-ARCH-01 (deep-dive-arch v1.1 priority #1) — the MCP/HTTP error-code drift smell.
**Date:** 2026-05-31
**Status:** approved (design); ready for implementation planning.

## 1. Problem

Clarion emits structured error codes on two independent wire surfaces, each with
its own hand-maintained, drift-prone taxonomy:

1. **HTTP federation read API** (`crates/clarion-cli/src/http_read.rs`) — a private
   `enum ErrorCode` with `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]`, 10
   variants, frozen as a wire contract in `docs/federation/contracts.md` and
   ADR-034. Switched on by Filigree / Wardline clients. Already typed; already has
   a partial `StorageError → (code, status)` classifier (`classify_read_error`).
2. **MCP tool-error envelope** (`crates/clarion-mcp/src/lib.rs`) — **bare kebab-case
   string literals** passed to `tool_error_envelope(code: &str, …)` at ~47 call
   sites. No enum, no exhaustiveness, no compiler protection against typos or
   silent divergence. 18 distinct codes. Carries a `retryable: bool` flag the HTTP
   envelope lacks. Documented as "stable error-code strings on the wire"; 6 codes
   are pinned by exact-match MCP tests.

The "drift" is the maintenance risk: two taxonomies that can silently diverge, no
single typed source of truth (the MCP side has *no* type at all), and docs that can
drift from code (the originating finding was a `CHANGELOG.md` `UNAUTHORIZED` vs
contract `UNAUTHENTICATED` mismatch).

### 1.1 Evidence that reframes the fix

Investigation surfaced three facts that rule out the naive "one enum, one spelling"
unification:

- **The two vocabularies are largely disjoint, not parallel.** HTTP carries
  transport concepts (`PATH_OUTSIDE_PROJECT`, `BRIEFING_BLOCKED`, `UNAUTHENTICATED`,
  `BATCH_TOO_LARGE`, `WRITE_DISABLED`, `PROJECT_MISMATCH`) that never appear in MCP;
  MCP carries tool concepts (`llm-disabled`, `token-ceiling-exceeded`,
  `analyze-already-running`, `spawn-failed`, `content-drift`, `inferred-dispatch-*`,
  …) that never appear in HTTP. The genuine overlap is four concepts — invalid
  path, storage error, not found, internal — and even there MCP wants finer
  granularity (`entity-not-found` / `run-not-found` / `content-hash-missing`) that
  HTTP collapses to a single `NOT_FOUND`.
- **HTTP status is intentionally decoupled from the code.** The contract mandates
  "same `code`, different HTTP status by endpoint" (`BATCH_TOO_LARGE` → 400 on the
  files batch route, 413 on the wardline batch routes; `INTERNAL` → 500 and also 408
  on request timeout). A total `ErrorCode::status_code()` therefore cannot exist; the
  per-endpoint status choice must stay at the call site.
- **Both spellings have real consumers that switch on them.** HTTP: Filigree /
  Wardline, frozen in contract + ADR-034. MCP: 6 pinned tests + consult-mode agents,
  documented stable-on-the-wire. Re-spelling either is churn that breaks a live
  contract for cosmetic uniformity, and uniform spelling does not reduce drift —
  drift dies from a single source of truth plus drift-tests, not from spelling
  parity.

## 2. Decision

**Co-locate, keep per-surface wire spellings.** A single `clarion-core::errors`
module becomes the source of truth for *both* typed vocabularies. We do **not**
merge them into one grab-bag enum and do **not** re-spell either surface.

Rejected alternative — **merge to one SCREAMING_SNAKE enum, migrate MCP off kebab**:
~22 mostly-disjoint variants where every HTTP handler is nominally able to return
`llm-disabled`; loses HTTP's closed-enum-as-contract property and MCP's finer
`NOT_FOUND` granularity; rewrites `contracts.md`, ADR-034, federation fixtures, and
the 6 pinned MCP tests — all to break two live contracts for spelling cosmetics that
do not reduce drift.

Co-locating captures every anti-drift benefit (single definition + pinned wire
strings + drift tests) with no contract breakage and additive-only doc changes, and
it delivers the highest-value part of the ticket: giving the MCP side a real type.

## 3. Design

### 3.1 New module `crates/clarion-core/src/errors.rs`

`clarion-core` already depends on `serde` and `thiserror`; no new dependencies.

**`HttpErrorCode`** — moved verbatim from `http_read.rs`:

- 10 variants, unchanged names: `InvalidPath, PathOutsideProject, NotFound,
  BriefingBlocked, Unauthenticated, StorageError, BatchTooLarge, WriteDisabled,
  ProjectMismatch, Internal`.
- `#[derive(Debug, Copy, Clone, Serialize)]` + `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]`.
  Wire output byte-identical to today.
- Per-variant doc comments carried over verbatim.
- Adds `pub fn as_str(&self) -> &'static str` returning the SCREAMING_SNAKE wire
  string (for logging and the drift test; the existing serde path is unchanged).

**`McpErrorCode`** — NEW, the 18 codes currently emitted as bare literals:

```
invalid-path            entity-not-found        storage-error
not-a-subsystem         not-found               spawn-failed
io-error                analyze-already-running run-not-found
llm-disabled            token-ceiling-exceeded  llm-provider-error
internal                llm-invalid-json        content-drift
content-hash-missing    inferred-dispatch-cancelled
inferred-dispatch-timeout
```

- `#[derive(Debug, Copy, Clone, PartialEq, Eq)]`.
- `pub fn as_str(&self) -> &'static str` returning the exact kebab-case wire string.
  The MCP envelope builds a `serde_json::Value` from `code.as_str()`, so the wire
  bytes are identical to today. (Serialize derive is optional and not required by
  any call path; `as_str()` is the single spelling authority.)
- `PartialEq` so the two existing `err.code == "<literal>"` comparisons in
  `lib.rs` become enum comparisons.

**Module rustdoc = the normative home.** Documents:

- The two surfaces and why they are separate (different transports, different
  consumers, different granularity).
- The disjointness and the four genuinely-overlapping concepts.
- The **MCP → HTTP narrowing relationship as a documentation table** — e.g.
  `entity-not-found` / `run-not-found` / `not-found` / `content-hash-missing` all
  narrow to HTTP `NOT_FOUND`; `invalid-path` ↔ `INVALID_PATH`; `storage-error` ↔
  `STORAGE_ERROR`; `internal` ↔ `INTERNAL`; the remaining MCP codes have no HTTP
  surface and the remaining HTTP codes have no MCP surface. This is **documentation,
  not a method**: nothing in the system converts an MCP code to an HTTP code, so a
  `to_http()` would be dead code (YAGNI). The table records the relationship for
  maintainers and is what a future reader consults to keep the surfaces coherent.

### 3.2 Crate wiring

- `crates/clarion-core/src/lib.rs`: add `pub mod errors;` and, per the
  clarion-29acbcd042 facade policy, re-export at the crate root:
  `pub use errors::{HttpErrorCode, McpErrorCode};`.

- `crates/clarion-cli/src/http_read.rs`: delete the local `enum ErrorCode` and its
  derives; add `use clarion_core::HttpErrorCode as ErrorCode;`. All 44 existing
  `ErrorCode::*` references compile unchanged via the alias. `classify_read_error`,
  the `ReadError` struct, and every per-call-site `StatusCode` choice stay in
  clarion-cli — they bind `StorageError` (clarion-storage) and `StatusCode`
  (axum/http), neither of which clarion-core sees. No wire change.

- `crates/clarion-mcp/src/lib.rs`:
  - `fn tool_error_envelope(code: McpErrorCode, message: &str, retryable: bool)` and
    `tool_error_envelope_with_diagnostics(code: McpErrorCode, …)`; the body inserts
    `code.as_str()` into the JSON.
  - `struct InferredDispatchFailure { code: McpErrorCode, … }`; `from_storage` sets
    `McpErrorCode::StorageError`; `to_envelope`'s `self.code == "token-ceiling-exceeded"`
    becomes `self.code == McpErrorCode::TokenCeilingExceeded`.
  - The `err.code == "llm-invalid-json"` comparison becomes the enum compare.
  - `token_ceiling_envelope` keeps emitting the same `"token-ceiling-exceeded"`
    string; route it through `McpErrorCode::TokenCeilingExceeded.as_str()` so the
    literal lives in exactly one place.
  - All ~47 call sites pass a variant instead of a string literal.
  - `storage_retryable(&StorageError)` stays in clarion-mcp unchanged (it depends on
    `StorageError`; `retryable` remains an explicit argument — behavior-preserving).

### 3.3 Out of scope (scope discipline)

- **`status_code()` on the enum** — dropped; a total code→status map is impossible
  under the per-endpoint-status contract (§1.1).
- **Merging vocabularies or unifying spelling** — rejected in §2.
- **Moving retryability onto the enum** — `retryable` stays an explicit argument;
  the hardcoded per-call defaults are not refactored (YAGNI, behavior-preserving).
- **Success-envelope `reason`/`kind` strings** — `filigree-disabled`,
  `filigree-unreachable`, `filigree-client-error` (the `issues_unavailable` `reason`
  field), `summary-scope-deferred`, `structural-fallback`, `guidance-empty` are not
  error codes and are not part of this vocabulary. Left untouched.
- **JSON-RPC framing errors** (`-32600/-32601/-32602`) — a separate numeric protocol
  layer; untouched.

## 4. Testing

- **Drift tests in `errors.rs`** (the anti-drift mechanism): one test asserts each
  `HttpErrorCode` variant serializes (via serde) to its exact SCREAMING_SNAKE wire
  string AND that `as_str()` agrees; one test asserts each `McpErrorCode::as_str()`
  returns its exact kebab string. These pin the wire form at the definition site, so
  any accidental rename fails a unit test next to the enum.
- **Existing MCP pinned tests** (`crates/clarion-mcp/tests/storage_tools.rs`) and any
  HTTP serve tests asserting SCREAMING_SNAKE strings pass **unchanged** — the wire
  bytes are byte-identical.
- **Full workspace gate** (CLAUDE.md floor) must pass: `cargo fmt --check`,
  `cargo clippy --workspace --all-targets --all-features -D warnings`,
  `cargo build --workspace --bins`, `cargo nextest run --workspace --all-features`,
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`,
  `cargo deny check`.

## 5. Documentation in lockstep

- **ADR-037** (new, short) — records the decision: co-locate two typed error
  vocabularies in `clarion-core::errors`; do not merge; keep per-surface wire
  spellings; status stays per-endpoint. Add to the ADR index / README.
- **`docs/federation/contracts.md`** — wire is unchanged; add an additive pointer
  noting the HTTP `code` enum is now defined canonically in `clarion_core::errors`.
- The arch-analysis snapshots (`docs/arch-analysis-*`) are historical and are **not**
  edited.

## 6. Acceptance

- `McpErrorCode` and `HttpErrorCode` both live in `clarion-core::errors`; both
  re-exported at the crate root.
- Zero bare error-code string literals remain at MCP `tool_error_envelope` call sites
  (each passes a variant).
- HTTP and MCP wire output is byte-identical to pre-change (verified by the existing
  pinned tests plus the new drift tests).
- ADR-037 written and indexed; `contracts.md` pointer added.
- Full CLAUDE.md gate green.
