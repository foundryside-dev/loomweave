# ADR-037: Shared Error Vocabulary (`clarion-core::errors`)

**Status**: Accepted
**Date**: 2026-05-31
**Deciders**: qacona@gmail.com
**Context**: Deep-dive architectural review V11-ARCH-01 (ticket clarion-b57c6bc49f) identified the ~47 bare MCP error string literals as priority #1 architectural gap: no type, no exhaustiveness, no protection against typos or wire drift.
**Relates to**: [ADR-034](./ADR-034-federation-http-read-api-hardening.md) (shares the HTTP error envelope frozen by ADR-034; ADR-034 remains Accepted and is not superseded by this ADR).

## Summary

Co-locate two **separate** typed enums — `HttpErrorCode` and `McpErrorCode` — in a new `clarion-core::errors` module as the single source of truth for Clarion's two structured wire error surfaces. Each enum preserves its established wire spelling (SCREAMING_SNAKE for HTTP, kebab-case for MCP). Wire bytes are unchanged on both surfaces. Drift tests pin every wire string at the definition site.

## Context

Clarion emits machine-routable error codes on two independent wire surfaces:

1. **Federation HTTP read API** — `HttpErrorCode`, SCREAMING_SNAKE on the wire (`INVALID_PATH`, `NOT_FOUND`, etc.). Contract frozen by ADR-034 and `docs/federation/contracts.md`; consumed by Filigree and Wardline clients that `switch on code`. The ten-variant set accrued across ADR-014 (initial error envelope), ADR-034 (added `BRIEFING_BLOCKED`, `UNAUTHENTICATED`, `BATCH_TOO_LARGE`), and ADR-036 (added `WRITE_DISABLED`, `PROJECT_MISMATCH` for the `/api/wardline/*` routes).

2. **MCP tool-error envelope** — consumed by consult-mode agents. Prior to this work: ~47 bare string literals scattered across `clarion-mcp`, no type, no exhaustiveness checking, no compiler protection against typos, and no pinning of the wire strings in tests.

The two surfaces serve different transports (HTTP vs. stdio/MCP), different consumers (Loom siblings vs. LLM agents), and different granularities (the MCP side carries 18 fine-grained codes while the HTTP side carries 10 coarser ones). Without a shared home, the vocabularies could drift independently and silently.

The finding was raised as V11-ARCH-01 in the v1.1 deep-dive architectural review and tracked as Filigree issue clarion-b57c6bc49f.

## Decision

Add a `clarion-core::errors` module containing two separate typed enums:

- **`HttpErrorCode`** — the closed SCREAMING_SNAKE HTTP error-code set. Serializes via `serde(rename_all = "SCREAMING_SNAKE_CASE")`. Wire spelling is unchanged (frozen ADR-034 contract). HTTP status is chosen per endpoint by the callers in `clarion-cli`; no total `code → status` mapping exists on the enum itself because the contract mandates that the same code can carry different HTTP statuses on different endpoints (e.g. `BATCH_TOO_LARGE` is 400 on `POST /api/v1/files/batch` per ADR-034 but 413 on the `/api/wardline/*` batch routes per ADR-036; see `docs/federation/contracts.md`). Drift tests assert that every `as_str()` return equals the corresponding `serde_json::to_string()` output.

- **`McpErrorCode`** — the closed kebab-case MCP error-code set. Wire spelling is unchanged from the previous bare string literals. Drift tests pin all 18 wire strings at the definition site.

The **MCP → HTTP narrowing relationship** is documented as a reference table in the module rustdoc (not as a conversion method — see Alternatives). The module rustdoc cites this ADR as the design record.

**Glossary gate:** This ADR introduces no new cross-product-visible wire term; both vocabularies pre-exist and wire spelling is unchanged on both surfaces. The ADR-README glossary criterion (`no clash` / `managed clash` / `renamed`) does not trigger; no `glossary.md` update is required.

## Alternatives Considered

### Alternative 1: Merge into a single SCREAMING_SNAKE enum; migrate MCP off kebab

A single combined enum would make the relationship between the surfaces explicit at the type level. Rejected on three grounds:

1. **Breaks both frozen wire contracts simultaneously** — migrating MCP off kebab would need every consult-mode agent that currently sends or receives MCP error codes to be updated; at the same time, adding `_` separators to the HTTP side would break the ADR-034 / contracts.md frozen HTTP contract and every sibling client compiled against it.
2. **Couples two largely-disjoint vocabularies** — the combined set would be a ~22-variant grab-bag with no structural relationship between the two halves. The MCP side's fine-grained distinctions (e.g. `entity-not-found` vs. `run-not-found` vs. `not-found`) would all need to survive in the merged enum alongside HTTP-only codes (`PATH_OUTSIDE_PROJECT`, `BRIEFING_BLOCKED`, `UNAUTHENTICATED`, `BATCH_TOO_LARGE`, `WRITE_DISABLED`, `PROJECT_MISMATCH`) and MCP-only codes (`not-a-subsystem`, `analyze-already-running`, `token-ceiling-exceeded`, `llm-disabled`, `llm-provider-error`, `llm-invalid-json`, `content-drift`, `inferred-dispatch-cancelled`, `inferred-dispatch-timeout`).
3. **Does not eliminate drift** — drift dies from a single source of truth combined with per-wire drift tests, not from spelling parity. Separate enums in one module achieve the same drift protection without rewriting wire contracts.

### Alternative 2: Add a `status_code()` → `StatusCode` method to `HttpErrorCode`

A total mapping from `HttpErrorCode` to an HTTP status code would require choosing one canonical status per code. Rejected: the federation contract (ADR-034, `contracts.md`) mandates per-endpoint status selection — `BATCH_TOO_LARGE` is 400 on one route and 413 on another. A total mapping is not just YAGNI; it would be wrong. Status selection stays at the call site in `http_read.rs`.

### Alternative 3: Add a `to_http()` narrowing method on `McpErrorCode`

A method converting a fine-grained MCP code to a coarse HTTP code would encode the narrowing relationship as a callable API. Rejected as dead code: the two surfaces are disjoint in the Clarion codebase — no production code path converts an MCP error to an HTTP response or vice versa. Dead methods accumulate maintenance cost without benefit. The narrowing relationship is recorded as a rustdoc table in the module instead, where it is a maintainer reference that does not compile to anything.

### Alternative 4: Keep bare string literals; add a test-only string set for pinning

Add a centralised list of expected strings in a test helper without creating enum types. Rejected: this still allows typos in the ~47 scattered call sites, does not give exhaustiveness checking, and provides no structural guarantee that a newly added code has been added to the pin set. The enum approach is strictly stronger.

## Consequences

### Positive

- **Single typed home for both vocabularies.** All additions, renames, and retirements require a one-line change in `clarion-core::errors` plus a test assertion, not a grep through `clarion-mcp`'s tool handlers.
- **MCP codes are now compiler-checked.** Exhaustive matches on `McpErrorCode` mean the compiler detects a missing branch. Typos at call sites are now a type error, not a silent wire divergence.
- **Wire bytes unchanged on both surfaces.** Existing HTTP tests (ADR-034 wire assertions) and the 6 pinned MCP wire tests pass unchanged. Neither Filigree, Wardline, nor any consult-mode agent needs updating.
- **ADR-034 / contracts.md HTTP wire contract untouched.** The `HttpErrorCode` enum is the same closed set; it gains no new variants and emits identical SCREAMING_SNAKE strings.

### Negative

- **`clarion-cli` and `clarion-mcp` now depend on `clarion-core`** for their error code types. This is a crate-internal dependency increase, not a federation coupling; `clarion-core` is already the shared crate for entity-ID logic, plugin host, and manifest parsing. The coupling cost is minimal.

### Neutral

- The `McpErrorCode` enum does not implement `serde::Serialize` (the MCP envelope is assembled by hand via `as_str()`, matching the historical pattern). If a future refactor moves to serde-driven serialization, the variant naming would need adjustment; that is tracked as an obvious follow-up, not a defect.
- The MCP → HTTP narrowing table lives in rustdoc on the module. It is a documentation artifact only, not tested — maintainers are responsible for keeping it current when codes are added.

## Related Decisions

- [ADR-034](./ADR-034-federation-http-read-api-hardening.md) — Federation HTTP read API hardening; extended the HTTP error-code set with `BRIEFING_BLOCKED`, `UNAUTHENTICATED`, and `BATCH_TOO_LARGE`. ADR-034 remains Accepted.
- [ADR-036](./ADR-036-wardline-taint-fact-store.md) — Wardline taint-fact store; added `WRITE_DISABLED` and `PROJECT_MISMATCH` for the `/api/wardline/*` read+write routes. `HttpErrorCode` is the typed representation of the full ten-code set across ADR-014, ADR-034, and ADR-036.
- [ADR-014](./ADR-014-filigree-registry-backend.md) — Original HTTP error envelope; later extended by ADR-034 and ADR-036.
- [ADR-023](./ADR-023-tooling-baseline.md) — Tooling baseline; the drift tests are validated under the nextest gate this ADR mandates.

## References

- Module: [`crates/clarion-core/src/errors.rs`](../../../crates/clarion-core/src/errors.rs)
- Wire contract: [`docs/federation/contracts.md`](../../federation/contracts.md) (pointer back to `HttpErrorCode` added addend in same commit)
- Implementing commits:
  - `5afbbe0` feat(errors): add `clarion-core::errors` with `HttpErrorCode` + `McpErrorCode`
  - `3701cc7` refactor(http_read): migrate to shared `HttpErrorCode`
  - `8f3e539` refactor(mcp): type MCP error codes off `McpErrorCode`; retire ~47 bare literals
