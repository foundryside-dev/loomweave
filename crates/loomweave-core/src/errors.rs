//! Shared error-code vocabularies for Loomweave's two structured wire surfaces.
//!
//! Loomweave emits machine-routable error codes on two **independent** surfaces.
//! This module is the single typed source of truth for both, so they cannot
//! silently drift and so the MCP side gets compiler-checked codes instead of
//! bare string literals. The two vocabularies are deliberately **not merged**:
//! they serve different transports, different consumers, and different
//! granularities (see the narrowing table below). HTTP status is intentionally
//! chosen per endpoint and is therefore *not* derivable from the code — see
//! `loomweave-cli`'s `classify_read_error` and ADR-037.
//!
//! # Surfaces
//!
//! * [`HttpErrorCode`] — the federation HTTP read API (`crates/loomweave-cli`).
//!   `SCREAMING_SNAKE` on the wire; frozen contract in `docs/federation/contracts.md`
//!   and ADR-034; switched on by Filigree / Wardline clients.
//! * [`McpErrorCode`] — the MCP tool-error envelope (`crates/loomweave-mcp`).
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
//! | `index-building`                                                        | `INDEX_BUILDING` |
//! | `index-build-failed`                                                    | `INDEX_BUILD_FAILED` / `BOOTSTRAP_SPAWN_FAILED` |
//! | `source-root-missing`                                                   | `SOURCE_ROOT_MISSING` |
//! | `not-a-subsystem`, `analyze-already-running`, `token-ceiling-exceeded`, `llm-disabled`, `llm-provider-error`, `llm-invalid-json`, `content-drift`, `inferred-dispatch-cancelled`, `inferred-dispatch-timeout` | *(MCP-only; no HTTP surface)* |
//!
//! HTTP-only codes (no MCP surface): `PATH_OUTSIDE_PROJECT`, `BRIEFING_BLOCKED`,
//! `UNAUTHENTICATED`, `BATCH_TOO_LARGE`, `WRITE_DISABLED`, `PROJECT_MISMATCH`,
//! and `BOOTSTRAP_SPAWN_FAILED` (a narrower transport diagnostic).

use serde::Serialize;

/// Closed error-code set for the federation HTTP read API.
///
/// Serializes to `SCREAMING_SNAKE_CASE`; this wire form is a frozen contract
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
    /// A linked worktree's isolated index has no completed analyze run yet —
    /// the HTTP read API is gated exactly like the MCP graph tools
    /// (`McpErrorCode::IndexBuilding`) so a federation consumer polling
    /// during the bootstrap build gets an explicit not-ready signal instead
    /// of well-formed empty answers indistinguishable from a truthfully
    /// empty index (clarion-ecf882f230). Carried with HTTP 503; retryable.
    IndexBuilding,
    /// The linked worktree's bootstrap analyze failed — HTTP analog of
    /// `McpErrorCode::IndexBuildFailed`. Carried with HTTP 503; not
    /// retryable without operator action (the response body names the
    /// fallback command).
    IndexBuildFailed,
    /// `serve` could not launch the linked worktree's bootstrap analyzer at
    /// all. Carried with HTTP 503 and non-retryable until the surfaced
    /// fallback command is run manually.
    BootstrapSpawnFailed,
    /// The linked worktree source root disappeared while `serve` remained
    /// alive. Carried with HTTP 410; stale graph rows are never served for a
    /// source tree that no longer exists.
    SourceRootMissing,
    Internal,
}

impl HttpErrorCode {
    /// The exact `SCREAMING_SNAKE` wire string (matches the `Serialize` output).
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
            Self::IndexBuilding => "INDEX_BUILDING",
            Self::IndexBuildFailed => "INDEX_BUILD_FAILED",
            Self::BootstrapSpawnFailed => "BOOTSTRAP_SPAWN_FAILED",
            Self::SourceRootMissing => "SOURCE_ROOT_MISSING",
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
    /// A linked worktree's isolated index has no completed analyze run yet —
    /// `serve`'s bootstrap spawned `loomweave worktree analyze` and it is
    /// still in flight (or has not yet written a `runs` row at all). The
    /// worktree-indexes design's per-call readiness consult (not file
    /// existence) is the source of truth for this state.
    IndexBuilding,
    /// A linked worktree's isolated index build ran and failed (`runs.status
    /// = 'failed'`). Diagnostics carry the exact `loomweave worktree analyze`
    /// fallback command to retry.
    IndexBuildFailed,
    /// The resolved source root for a live `serve` session no longer exists
    /// on disk (e.g. `git worktree remove` ran under a still-running serve).
    /// Surfaced instead of the last staleness verdict — the design's
    /// accepted race for worktree removal under a live session.
    SourceRootMissing,
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
            Self::IndexBuilding => "index-building",
            Self::IndexBuildFailed => "index-build-failed",
            Self::SourceRootMissing => "source-root-missing",
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
            HttpErrorCode::IndexBuilding,
            HttpErrorCode::IndexBuildFailed,
            HttpErrorCode::BootstrapSpawnFailed,
            HttpErrorCode::SourceRootMissing,
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
        assert_eq!(
            HttpErrorCode::PathOutsideProject.as_str(),
            "PATH_OUTSIDE_PROJECT"
        );
        assert_eq!(HttpErrorCode::NotFound.as_str(), "NOT_FOUND");
        assert_eq!(HttpErrorCode::BriefingBlocked.as_str(), "BRIEFING_BLOCKED");
        assert_eq!(HttpErrorCode::Unauthenticated.as_str(), "UNAUTHENTICATED");
        assert_eq!(HttpErrorCode::StorageError.as_str(), "STORAGE_ERROR");
        assert_eq!(HttpErrorCode::BatchTooLarge.as_str(), "BATCH_TOO_LARGE");
        assert_eq!(HttpErrorCode::WriteDisabled.as_str(), "WRITE_DISABLED");
        assert_eq!(HttpErrorCode::ProjectMismatch.as_str(), "PROJECT_MISMATCH");
        assert_eq!(HttpErrorCode::IndexBuilding.as_str(), "INDEX_BUILDING");
        assert_eq!(
            HttpErrorCode::IndexBuildFailed.as_str(),
            "INDEX_BUILD_FAILED"
        );
        assert_eq!(
            HttpErrorCode::BootstrapSpawnFailed.as_str(),
            "BOOTSTRAP_SPAWN_FAILED"
        );
        assert_eq!(
            HttpErrorCode::SourceRootMissing.as_str(),
            "SOURCE_ROOT_MISSING"
        );
        assert_eq!(HttpErrorCode::Internal.as_str(), "INTERNAL");
    }

    #[test]
    fn mcp_error_code_wire_strings_are_pinned() {
        // These kebab spellings are stable on the MCP wire (pinned by
        // loomweave-mcp/tests/storage_tools.rs and relied on by consult agents).
        assert_eq!(McpErrorCode::InvalidPath.as_str(), "invalid-path");
        assert_eq!(McpErrorCode::EntityNotFound.as_str(), "entity-not-found");
        assert_eq!(McpErrorCode::StorageError.as_str(), "storage-error");
        assert_eq!(McpErrorCode::NotASubsystem.as_str(), "not-a-subsystem");
        assert_eq!(McpErrorCode::NotFound.as_str(), "not-found");
        assert_eq!(McpErrorCode::SpawnFailed.as_str(), "spawn-failed");
        assert_eq!(McpErrorCode::IoError.as_str(), "io-error");
        assert_eq!(
            McpErrorCode::AnalyzeAlreadyRunning.as_str(),
            "analyze-already-running"
        );
        assert_eq!(McpErrorCode::RunNotFound.as_str(), "run-not-found");
        assert_eq!(McpErrorCode::LlmDisabled.as_str(), "llm-disabled");
        assert_eq!(
            McpErrorCode::TokenCeilingExceeded.as_str(),
            "token-ceiling-exceeded"
        );
        assert_eq!(
            McpErrorCode::LlmProviderError.as_str(),
            "llm-provider-error"
        );
        assert_eq!(McpErrorCode::Internal.as_str(), "internal");
        assert_eq!(McpErrorCode::LlmInvalidJson.as_str(), "llm-invalid-json");
        assert_eq!(McpErrorCode::ContentDrift.as_str(), "content-drift");
        assert_eq!(
            McpErrorCode::ContentHashMissing.as_str(),
            "content-hash-missing"
        );
        assert_eq!(
            McpErrorCode::InferredDispatchCancelled.as_str(),
            "inferred-dispatch-cancelled"
        );
        assert_eq!(
            McpErrorCode::InferredDispatchTimeout.as_str(),
            "inferred-dispatch-timeout"
        );
        assert_eq!(McpErrorCode::IndexBuilding.as_str(), "index-building");
        assert_eq!(
            McpErrorCode::IndexBuildFailed.as_str(),
            "index-build-failed"
        );
        assert_eq!(
            McpErrorCode::SourceRootMissing.as_str(),
            "source-root-missing"
        );
    }
}
