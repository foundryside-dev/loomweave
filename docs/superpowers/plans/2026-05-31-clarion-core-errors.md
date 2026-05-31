# clarion-core::errors Shared Error Vocabulary — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `clarion-core::errors` the single typed source of truth for both error-code wire vocabularies (HTTP federation + MCP tool envelope), killing the drift smell (V11-ARCH-01) without changing any wire bytes.

**Architecture:** Co-locate two *separate* typed enums — `HttpErrorCode` (moved verbatim from `http_read.rs`) and a new `McpErrorCode` (replacing ~47 bare kebab string literals) — in one `clarion-core` module, with drift tests pinning every wire string. Spellings stay per-surface; nothing is merged. HTTP status stays per-call-site. See `docs/superpowers/specs/2026-05-31-clarion-core-errors-design.md`.

**Tech Stack:** Rust workspace; `serde` (already a `clarion-core` dep); `thiserror`; `cargo nextest`; `clippy -D warnings`.

**Project gate (every commit should leave this green; Task 6 verifies the whole thing):**
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
```
Note: shell startup on this machine is slow; expect Bash output to batch.

**Refactor-TDD note:** Tasks 1 and 3 *move/retype* existing code rather than add net-new behavior. The "test" for each is the drift test (pins the wire string) plus the existing pinned wire tests staying byte-identical. Write the drift test in the same task as the enum; run the full crate test to prove no wire regression.

---

## File map

- **Create:** `crates/clarion-core/src/errors.rs` — both enums, `as_str()` on each, module rustdoc (incl. the MCP→HTTP narrowing table), drift tests.
- **Modify:** `crates/clarion-core/src/lib.rs` — `pub mod errors;` + facade re-export.
- **Modify:** `crates/clarion-cli/src/http_read.rs` — delete local `enum ErrorCode`; alias to `clarion_core::HttpErrorCode`.
- **Modify:** `crates/clarion-mcp/src/lib.rs` — retype `tool_error_envelope*` + `InferredDispatchFailure.code`; convert ~47 call sites; two `== "<literal>"` comparisons; `token_ceiling_envelope` literal.
- **Create:** `docs/clarion/adr/ADR-037-shared-error-vocabulary.md`; **Modify:** ADR index + `docs/federation/contracts.md` (additive pointer).

---

## Task 1: Create `clarion-core::errors` with both enums + drift tests

**Files:**
- Create: `crates/clarion-core/src/errors.rs`
- Modify: `crates/clarion-core/src/lib.rs:9-13` (add module + re-export)

- [ ] **Step 1: Create `errors.rs` with both enums, `as_str()`, rustdoc, and drift tests**

Create `crates/clarion-core/src/errors.rs` with exactly this content:

```rust
//! Shared error-code vocabularies for Clarion's two structured wire surfaces.
//!
//! Clarion emits machine-routable error codes on two **independent** surfaces.
//! This module is the single typed source of truth for both, so they cannot
//! silently drift and so the MCP side gets compiler-checked codes instead of
//! bare string literals. The two vocabularies are deliberately **not merged**:
//! they serve different transports, different consumers, and different
//! granularities (see the narrowing table below). HTTP status is intentionally
//! chosen per endpoint and is therefore *not* derivable from the code — see
//! `clarion-cli`'s `classify_read_error` and ADR-037.
//!
//! # Surfaces
//!
//! * [`HttpErrorCode`] — the federation HTTP read API (`crates/clarion-cli`).
//!   SCREAMING_SNAKE on the wire; frozen contract in `docs/federation/contracts.md`
//!   and ADR-034; switched on by Filigree / Wardline clients.
//! * [`McpErrorCode`] — the MCP tool-error envelope (`crates/clarion-mcp`).
//!   kebab-case on the wire; consumed by consult-mode agents; pinned by tests.
//!
//! # MCP → HTTP narrowing relationship (documentation only)
//!
//! Nothing in the system converts an MCP code to an HTTP code — the surfaces are
//! disjoint — so this is a maintainer reference, not a method. Where the concepts
//! overlap, several fine-grained MCP codes narrow to one coarse HTTP code:
//!
//! | MCP code(s)                                                            | HTTP code        |
//! |-----------------------------------------------------------------------|------------------|
//! | `invalid-path`                                                         | `INVALID_PATH`   |
//! | `storage-error`                                                        | `STORAGE_ERROR`  |
//! | `internal`, `io-error`, `spawn-failed`                                 | `INTERNAL`       |
//! | `entity-not-found`, `run-not-found`, `not-found`, `content-hash-missing` | `NOT_FOUND`    |
//! | `not-a-subsystem`, `analyze-already-running`, `token-ceiling-exceeded`, `llm-disabled`, `llm-provider-error`, `llm-invalid-json`, `content-drift`, `inferred-dispatch-cancelled`, `inferred-dispatch-timeout` | *(MCP-only; no HTTP surface)* |
//!
//! HTTP-only codes (no MCP surface): `PATH_OUTSIDE_PROJECT`, `BRIEFING_BLOCKED`,
//! `UNAUTHENTICATED`, `BATCH_TOO_LARGE`, `WRITE_DISABLED`, `PROJECT_MISMATCH`.

use serde::Serialize;

/// Closed error-code set for the federation HTTP read API.
///
/// Serializes to SCREAMING_SNAKE_CASE; this wire form is a frozen contract
/// (`docs/federation/contracts.md`, ADR-034). The HTTP status carried alongside
/// a given code is chosen per endpoint and is **not** a function of the code.
#[derive(Debug, Copy, Clone, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HttpErrorCode {
    InvalidPath,
    PathOutsideProject,
    NotFound,
    BriefingBlocked,
    Unauthenticated,
    StorageError,
    BatchTooLarge,
    /// Constructed by the write endpoint (`POST /api/wardline/taint-facts`)
    /// when the writer-actor is not enabled. Reachable only via
    /// `json_error(StatusCode::FORBIDDEN, …)`; no central `StatusCode` mapping
    /// is required.
    WriteDisabled,
    /// The `project` request guard did not match the served project.
    ProjectMismatch,
    Internal,
}

impl HttpErrorCode {
    /// The exact SCREAMING_SNAKE wire string (matches the `Serialize` output).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidPath => "INVALID_PATH",
            Self::PathOutsideProject => "PATH_OUTSIDE_PROJECT",
            Self::NotFound => "NOT_FOUND",
            Self::BriefingBlocked => "BRIEFING_BLOCKED",
            Self::Unauthenticated => "UNAUTHENTICATED",
            Self::StorageError => "STORAGE_ERROR",
            Self::BatchTooLarge => "BATCH_TOO_LARGE",
            Self::WriteDisabled => "WRITE_DISABLED",
            Self::ProjectMismatch => "PROJECT_MISMATCH",
            Self::Internal => "INTERNAL",
        }
    }
}

/// Closed error-code set for the MCP tool-error envelope.
///
/// `as_str` is the single authority for each code's kebab-case wire spelling;
/// the envelope builder inserts `as_str()` into the JSON, so the wire bytes are
/// identical to the historical bare string literals.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum McpErrorCode {
    InvalidPath,
    EntityNotFound,
    StorageError,
    NotASubsystem,
    NotFound,
    SpawnFailed,
    IoError,
    AnalyzeAlreadyRunning,
    RunNotFound,
    LlmDisabled,
    TokenCeilingExceeded,
    LlmProviderError,
    Internal,
    LlmInvalidJson,
    ContentDrift,
    ContentHashMissing,
    InferredDispatchCancelled,
    InferredDispatchTimeout,
}

impl McpErrorCode {
    /// The exact kebab-case wire string.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidPath => "invalid-path",
            Self::EntityNotFound => "entity-not-found",
            Self::StorageError => "storage-error",
            Self::NotASubsystem => "not-a-subsystem",
            Self::NotFound => "not-found",
            Self::SpawnFailed => "spawn-failed",
            Self::IoError => "io-error",
            Self::AnalyzeAlreadyRunning => "analyze-already-running",
            Self::RunNotFound => "run-not-found",
            Self::LlmDisabled => "llm-disabled",
            Self::TokenCeilingExceeded => "token-ceiling-exceeded",
            Self::LlmProviderError => "llm-provider-error",
            Self::Internal => "internal",
            Self::LlmInvalidJson => "llm-invalid-json",
            Self::ContentDrift => "content-drift",
            Self::ContentHashMissing => "content-hash-missing",
            Self::InferredDispatchCancelled => "inferred-dispatch-cancelled",
            Self::InferredDispatchTimeout => "inferred-dispatch-timeout",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_error_code_as_str_matches_serialize_output() {
        let all = [
            HttpErrorCode::InvalidPath,
            HttpErrorCode::PathOutsideProject,
            HttpErrorCode::NotFound,
            HttpErrorCode::BriefingBlocked,
            HttpErrorCode::Unauthenticated,
            HttpErrorCode::StorageError,
            HttpErrorCode::BatchTooLarge,
            HttpErrorCode::WriteDisabled,
            HttpErrorCode::ProjectMismatch,
            HttpErrorCode::Internal,
        ];
        for code in all {
            let serialized = serde_json::to_string(&code).expect("serializes");
            // serde renders the variant as a quoted JSON string.
            assert_eq!(serialized, format!("\"{}\"", code.as_str()));
        }
    }

    #[test]
    fn http_error_code_wire_strings_are_pinned() {
        // The federation contract (contracts.md, ADR-034) freezes these spellings.
        assert_eq!(HttpErrorCode::InvalidPath.as_str(), "INVALID_PATH");
        assert_eq!(HttpErrorCode::PathOutsideProject.as_str(), "PATH_OUTSIDE_PROJECT");
        assert_eq!(HttpErrorCode::NotFound.as_str(), "NOT_FOUND");
        assert_eq!(HttpErrorCode::BriefingBlocked.as_str(), "BRIEFING_BLOCKED");
        assert_eq!(HttpErrorCode::Unauthenticated.as_str(), "UNAUTHENTICATED");
        assert_eq!(HttpErrorCode::StorageError.as_str(), "STORAGE_ERROR");
        assert_eq!(HttpErrorCode::BatchTooLarge.as_str(), "BATCH_TOO_LARGE");
        assert_eq!(HttpErrorCode::WriteDisabled.as_str(), "WRITE_DISABLED");
        assert_eq!(HttpErrorCode::ProjectMismatch.as_str(), "PROJECT_MISMATCH");
        assert_eq!(HttpErrorCode::Internal.as_str(), "INTERNAL");
    }

    #[test]
    fn mcp_error_code_wire_strings_are_pinned() {
        // These kebab spellings are stable on the MCP wire (pinned by
        // clarion-mcp/tests/storage_tools.rs and relied on by consult agents).
        assert_eq!(McpErrorCode::InvalidPath.as_str(), "invalid-path");
        assert_eq!(McpErrorCode::EntityNotFound.as_str(), "entity-not-found");
        assert_eq!(McpErrorCode::StorageError.as_str(), "storage-error");
        assert_eq!(McpErrorCode::NotASubsystem.as_str(), "not-a-subsystem");
        assert_eq!(McpErrorCode::NotFound.as_str(), "not-found");
        assert_eq!(McpErrorCode::SpawnFailed.as_str(), "spawn-failed");
        assert_eq!(McpErrorCode::IoError.as_str(), "io-error");
        assert_eq!(McpErrorCode::AnalyzeAlreadyRunning.as_str(), "analyze-already-running");
        assert_eq!(McpErrorCode::RunNotFound.as_str(), "run-not-found");
        assert_eq!(McpErrorCode::LlmDisabled.as_str(), "llm-disabled");
        assert_eq!(McpErrorCode::TokenCeilingExceeded.as_str(), "token-ceiling-exceeded");
        assert_eq!(McpErrorCode::LlmProviderError.as_str(), "llm-provider-error");
        assert_eq!(McpErrorCode::Internal.as_str(), "internal");
        assert_eq!(McpErrorCode::LlmInvalidJson.as_str(), "llm-invalid-json");
        assert_eq!(McpErrorCode::ContentDrift.as_str(), "content-drift");
        assert_eq!(McpErrorCode::ContentHashMissing.as_str(), "content-hash-missing");
        assert_eq!(McpErrorCode::InferredDispatchCancelled.as_str(), "inferred-dispatch-cancelled");
        assert_eq!(McpErrorCode::InferredDispatchTimeout.as_str(), "inferred-dispatch-timeout");
    }
}
```

- [ ] **Step 2: Wire the module into `lib.rs`**

In `crates/clarion-core/src/lib.rs`, add `pub mod errors;` to the module list (after `pub mod entity_id;`, keeping alphabetical-ish order) and add the facade re-export. The module list near line 9 becomes:

```rust
pub mod entity_id;
pub mod errors;
pub mod llm_provider;
pub mod plugin;
```

And add, alongside the other `pub use` facade lines (e.g. right after the `pub use entity_id::{…};` line near line 13):

```rust
pub use errors::{HttpErrorCode, McpErrorCode};
```

- [ ] **Step 3: Verify the new module compiles and the drift tests pass**

Run:
```bash
cargo nextest run -p clarion-core errors::tests
```
Expected: 3 tests pass (`http_error_code_as_str_matches_serialize_output`, `http_error_code_wire_strings_are_pinned`, `mcp_error_code_wire_strings_are_pinned`).

- [ ] **Step 4: Verify clippy is clean (pub re-export means no dead-code warnings)**

Run:
```bash
cargo clippy -p clarion-core --all-targets --all-features -- -D warnings
```
Expected: no warnings, no errors. (The enums are `pub` and re-exported, so unused-by-consumers is not flagged.)

- [ ] **Step 5: Commit**

```bash
git add crates/clarion-core/src/errors.rs crates/clarion-core/src/lib.rs
git commit -m "feat(core): add clarion-core::errors with HttpErrorCode + McpErrorCode

Single typed source of truth for both wire error vocabularies; drift
tests pin every wire string. Consumers migrate in follow-ups (V11-ARCH-01).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Migrate the HTTP read API to `clarion_core::HttpErrorCode`

**Files:**
- Modify: `crates/clarion-cli/src/http_read.rs:728-746` (delete local enum), add a `use` alias.

- [ ] **Step 1: Delete the local enum and alias to the shared one**

In `crates/clarion-cli/src/http_read.rs`, delete the entire local definition (lines 728-746):

```rust
#[derive(Debug, Copy, Clone, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ErrorCode {
    InvalidPath,
    // … through …
    Internal,
}
```

Add, near the top of the file with the other `use` items (e.g. beside the existing `clarion_core` / `clarion_storage` imports):

```rust
use clarion_core::HttpErrorCode as ErrorCode;
```

All existing `ErrorCode::Variant` references (44 of them) compile unchanged through the alias. The `#[derive(Serialize)]` and SCREAMING_SNAKE rename now live on the shared type, so `ErrorResponse` / `BatchErrorItem` serialize identically.

- [ ] **Step 2: Verify the crate builds**

Run:
```bash
cargo build -p clarion-cli --bins
```
Expected: builds. If the compiler reports an unused `Serialize` import in `http_read.rs` (the local enum was its only user there), remove that now-unused import to satisfy `-D warnings`; if other types in the file still derive `Serialize`, leave it.

- [ ] **Step 3: Verify clippy + existing HTTP tests pass with byte-identical wire**

Run:
```bash
cargo clippy -p clarion-cli --all-targets --all-features -- -D warnings
cargo nextest run -p clarion-cli
```
Expected: clippy clean; all existing http_read tests pass (they assert the SCREAMING_SNAKE strings, which are unchanged).

- [ ] **Step 4: Commit**

```bash
git add crates/clarion-cli/src/http_read.rs
git commit -m "refactor(cli): use clarion_core::HttpErrorCode in http_read

Removes the duplicate local ErrorCode enum; wire output unchanged (V11-ARCH-01).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Migrate the MCP tool-error envelope to `McpErrorCode`

**Files:**
- Modify: `crates/clarion-mcp/src/lib.rs` — envelope fns, `InferredDispatchFailure`, ~47 call sites, two string comparisons, `token_ceiling_envelope`.

This task is atomic: changing the `code` parameter type forces every call site in the same commit. Work through it in order, then build once.

- [ ] **Step 1: Import the shared enum**

In `crates/clarion-mcp/src/lib.rs`, add to the `clarion_core` import group near the top:

```rust
use clarion_core::McpErrorCode;
```

- [ ] **Step 2: Retype the envelope builders to take `McpErrorCode`**

Change `tool_error_envelope` (≈ line 4018) and `tool_error_envelope_with_diagnostics` (≈ line 4022). New signatures + the JSON build line:

```rust
fn tool_error_envelope(code: McpErrorCode, message: &str, retryable: bool) -> Value {
    tool_error_envelope_with_diagnostics(code, message, retryable, json!({}), Vec::new())
}

fn tool_error_envelope_with_diagnostics(
    code: McpErrorCode,
    message: &str,
    retryable: bool,
    stats_delta: Value,
    diagnostics: Vec<Value>,
) -> Value {
    let mut envelope = serde_json::Map::new();
    envelope.insert("ok".to_owned(), Value::Bool(false));
    envelope.insert("result".to_owned(), Value::Null);
    envelope.insert(
        "error".to_owned(),
        json!({
            "code": code.as_str(),
            "message": message,
            "retryable": retryable,
        }),
    );
    envelope.insert("diagnostics".to_owned(), Value::Array(diagnostics));
    envelope.insert("truncated".to_owned(), Value::Bool(false));
    envelope.insert("truncation_reason".to_owned(), Value::Null);
    envelope.insert("stats_delta".to_owned(), stats_delta);
    Value::Object(envelope)
}
```

- [ ] **Step 3: Retype `InferredDispatchFailure.code` and its impl**

Change the struct (≈ line 3401), `new` (≈ 3411), `from_storage` (≈ 3421), and `to_envelope` (≈ 3441):

```rust
#[derive(Debug, Clone)]
struct InferredDispatchFailure {
    code: McpErrorCode,
    message: String,
    retryable: bool,
    stats_delta: Value,
    diagnostics: Vec<Value>,
}

impl InferredDispatchFailure {
    fn new(code: McpErrorCode, message: &str, retryable: bool) -> Self {
        Self {
            code,
            message: message.to_owned(),
            retryable,
            stats_delta: json!({}),
            diagnostics: Vec::new(),
        }
    }

    fn from_storage(err: &StorageError) -> Self {
        Self {
            code: McpErrorCode::StorageError,
            message: err.to_string(),
            retryable: !err.is_foreign_key_violation(),
            stats_delta: json!({}),
            diagnostics: Vec::new(),
        }
    }
    // with_stats unchanged …

    fn to_envelope(&self) -> Value {
        if self.code == McpErrorCode::TokenCeilingExceeded {
            return token_ceiling_envelope(&self.message);
        }
        tool_error_envelope_with_diagnostics(
            self.code,
            &self.message,
            self.retryable,
            self.stats_delta.clone(),
            self.diagnostics.clone(),
        )
    }
}
```

(Keep the `from_storage` doc comment that explains the FK→non-retryable rationale.)

- [ ] **Step 4: Fix the `llm-invalid-json` string comparison (≈ line 2614)**

Change:
```rust
Err(err) if err.code == "llm-invalid-json" => {
```
to:
```rust
Err(err) if err.code == McpErrorCode::LlmInvalidJson => {
```

- [ ] **Step 5: Route the `token_ceiling_envelope` literal through the enum (≈ line 4114)**

In `token_ceiling_envelope`, change the hand-built `"code": "token-ceiling-exceeded"` to use the single authority. Replace the `error` object's code line:
```rust
        "error": {
            "code": McpErrorCode::TokenCeilingExceeded.as_str(),
            "message": message,
            "retryable": false
        },
```
Leave the **diagnostics** `"code": "CLA-LLM-TOKEN-CEILING-EXCEEDED"` line unchanged — that is a finding subcode, not a tool-error code.

- [ ] **Step 6: Convert every `tool_error_envelope(...)` / `InferredDispatchFailure::new(...)` call site to a variant**

Replace each bare string literal in the first argument with its variant per this exhaustive map (the only literals that appear in those positions):

| literal | variant |
|---|---|
| `"invalid-path"` | `McpErrorCode::InvalidPath` |
| `"entity-not-found"` | `McpErrorCode::EntityNotFound` |
| `"storage-error"` | `McpErrorCode::StorageError` |
| `"not-a-subsystem"` | `McpErrorCode::NotASubsystem` |
| `"not-found"` | `McpErrorCode::NotFound` |
| `"spawn-failed"` | `McpErrorCode::SpawnFailed` |
| `"io-error"` | `McpErrorCode::IoError` |
| `"analyze-already-running"` | `McpErrorCode::AnalyzeAlreadyRunning` |
| `"run-not-found"` | `McpErrorCode::RunNotFound` |
| `"llm-disabled"` | `McpErrorCode::LlmDisabled` |
| `"token-ceiling-exceeded"` | `McpErrorCode::TokenCeilingExceeded` |
| `"llm-provider-error"` | `McpErrorCode::LlmProviderError` |
| `"internal"` | `McpErrorCode::Internal` |
| `"llm-invalid-json"` | `McpErrorCode::LlmInvalidJson` |
| `"content-drift"` | `McpErrorCode::ContentDrift` |
| `"content-hash-missing"` | `McpErrorCode::ContentHashMissing` |
| `"inferred-dispatch-cancelled"` | `McpErrorCode::InferredDispatchCancelled` |
| `"inferred-dispatch-timeout"` | `McpErrorCode::InferredDispatchTimeout` |

**Do NOT touch** these — they are not error codes (success-envelope `reason`/`kind`/fingerprint fields or finding subcodes): `"filigree-disabled"`, `"filigree-unreachable"`, `"filigree-client-error"` (the `issues_unavailable` `reason` arg), `"summary-scope-deferred"`, `"structural-fallback"`, `"guidance-empty"`, and any `"CLA-*"` diagnostic codes.

Find the call sites:
```bash
grep -n "tool_error_envelope\|tool_error_envelope_with_diagnostics\|InferredDispatchFailure::new" crates/clarion-mcp/src/lib.rs
```

- [ ] **Step 7: Build the crate (this is the gate that proves all call sites were converted)**

Run:
```bash
cargo build -p clarion-mcp
```
Expected: builds. Any remaining bare-string call site fails as a type error (`expected McpErrorCode, found &str`) naming the exact line — fix it using the table above and rebuild.

- [ ] **Step 8: Run clippy + the full MCP test suite (proves byte-identical wire)**

Run:
```bash
cargo clippy -p clarion-mcp --all-targets --all-features -- -D warnings
cargo nextest run -p clarion-mcp
```
Expected: clippy clean; all tests pass, including the pinned wire-string assertions in `crates/clarion-mcp/tests/storage_tools.rs` (`not-a-subsystem`, `llm-disabled`, `content-drift`, `token-ceiling-exceeded`, `not-found`, `llm-invalid-json`) — unchanged because `as_str()` reproduces each literal exactly.

- [ ] **Step 9: Commit**

```bash
git add crates/clarion-mcp/src/lib.rs
git commit -m "refactor(mcp): type tool-error codes with clarion_core::McpErrorCode

Replaces ~47 bare kebab string literals with compiler-checked variants;
wire output byte-identical (V11-ARCH-01).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: ADR-037 + contracts.md pointer

**Files:**
- Create: `docs/clarion/adr/ADR-037-shared-error-vocabulary.md`
- Modify: ADR index (`docs/clarion/adr/README.md` if present; otherwise `docs/clarion/1.0/README.md` ADR list)
- Modify: `docs/federation/contracts.md` (additive pointer near the error-envelope section, ~line 95)

- [ ] **Step 1: Inspect an existing ADR for the house format**

Run:
```bash
sed -n '1,40p' docs/clarion/adr/ADR-034-federation-http-read-api-hardening.md
ls docs/clarion/adr/ | grep -i readme
```
Match that header/status/section style in the next step.

- [ ] **Step 2: Write ADR-037**

Create `docs/clarion/adr/ADR-037-shared-error-vocabulary.md` following the house format observed in Step 1. Required content:

- **Status:** Accepted (2026-05-31).
- **Context:** Two independent error-code wire surfaces (HTTP federation, MCP tool envelope) with hand-maintained taxonomies; the MCP side had no type at all (~47 bare kebab literals); drift risk between code and docs. Cite V11-ARCH-01 and ticket clarion-b57c6bc49f.
- **Decision:** Co-locate two *separate* typed enums (`HttpErrorCode`, `McpErrorCode`) in `clarion-core::errors` as the single source of truth; keep each surface's wire spelling (SCREAMING_SNAKE / kebab); do **not** merge the vocabularies; HTTP status stays per-endpoint (a total code→status map is impossible under the "same code, different status by endpoint" contract — ADR-034). Drift tests pin every wire string at the definition site.
- **Alternatives rejected:** Merge to one SCREAMING_SNAKE enum + migrate MCP (breaks the frozen HTTP contract *and* pinned MCP strings, couples disjoint vocabularies, no drift benefit over co-location). `status_code()` on the enum (impossible per ADR-034). A `to_http()` narrowing method (dead code; documented as a table instead).
- **Consequences:** Single typed home; MCP codes compiler-checked; wire bytes unchanged; ADR-034 / contracts.md wire contract untouched.
- Note that this **partially relates to ADR-034** (shares the HTTP error envelope) without superseding it.

- [ ] **Step 3: Add ADR-037 to the index**

Add a one-line entry for ADR-037 to whichever index Step 1 found (the ADR `README.md`, or the ADR list in `docs/clarion/1.0/README.md`), matching the existing line format. If `CLAUDE.md`'s "28 Accepted" count enumerates ADR ranges, leave that prose alone (out of scope) unless it lists ADR-037's range explicitly.

- [ ] **Step 4: Add the additive pointer to contracts.md**

In `docs/federation/contracts.md`, just after the closed-enum sentence (~lines 95-104, "Clients must switch on `code`…"), add one sentence (wire unchanged):

```markdown
> The `code` enum is defined canonically in Rust as `clarion_core::errors::HttpErrorCode`
> (single source of truth shared with the MCP tool-error vocabulary; see ADR-037).
> The wire spelling on this surface is unchanged.
```

- [ ] **Step 5: Verify docs build clean (rustdoc intralinks in errors.rs)**

Run:
```bash
RUSTDOCFLAGS="-D warnings" cargo doc -p clarion-core --no-deps --all-features
```
Expected: builds with no warnings (catches any broken intra-doc link in the `errors.rs` rustdoc).

- [ ] **Step 6: Commit**

```bash
git add docs/clarion/adr/ADR-037-shared-error-vocabulary.md docs/federation/contracts.md docs/clarion/adr/README.md docs/clarion/1.0/README.md
git commit -m "docs(adr): ADR-037 shared error vocabulary; contracts.md pointer (V11-ARCH-01)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```
(Drop any path from the `git add` that you did not actually modify.)

---

## Task 5: Full-workspace gate

**Files:** none (verification only).

- [ ] **Step 1: Run the complete project gate**

Run each; all must pass:
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
```
Expected: all green. If `cargo fmt --all -- --check` reports diffs, run `cargo fmt --all`, review, and commit the formatting fixups:
```bash
git add -A && git commit -m "style: cargo fmt after error-vocabulary extraction"
```

- [ ] **Step 2: Confirm no bare error-code literals remain at MCP envelope call sites**

Run:
```bash
grep -nE 'tool_error_envelope(_with_diagnostics)?\(\s*"' crates/clarion-mcp/src/lib.rs
grep -nE 'InferredDispatchFailure::new\(\s*"' crates/clarion-mcp/src/lib.rs
```
Expected: no matches (every first argument is now a `McpErrorCode` variant).

---

## Done criteria

- [ ] `HttpErrorCode` + `McpErrorCode` live in `clarion-core::errors`, re-exported at the crate root.
- [ ] `http_read.rs` aliases the shared `HttpErrorCode`; no local error enum remains.
- [ ] Zero bare error-code string literals at MCP `tool_error_envelope*` / `InferredDispatchFailure::new` call sites.
- [ ] Drift tests pin every wire string; existing HTTP + 6 pinned MCP wire tests pass unchanged.
- [ ] ADR-037 written + indexed; contracts.md pointer added.
- [ ] Full workspace gate green.
- [ ] Ticket clarion-b57c6bc49f closed.
