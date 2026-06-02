# ADR-039: LLM provider pivot — OpenRouter + CLI providers; supersede CON-ANTHROPIC-01

**Status**: Accepted
**Date**: 2026-06-02
**Deciders**: qacona@gmail.com (with Claude)
**Context**: `CON-ANTHROPIC-01` (requirements.md) baselined Clarion's LLM provider as **Anthropic only** for v0.1, on the premise that Anthropic's explicit four-`cache_control`-breakpoint prompt caching was the mechanism that made elspeth-scale cost tractable, and that any other provider "either loses caching advantage or requires prompt-protocol refactoring (v0.3+)." The implementation diverged: `crates/clarion-core/src/llm_provider.rs` ships **four providers, none of which is a native Anthropic SDK provider**, and all of which declare `CachingModel::OpenAiChatCompletions`. This ADR records the pivot and supersedes `CON-ANTHROPIC-01`.
**Relates to**: [ADR-030](./ADR-030-on-demand-summary-scope.md) (on-demand summary scope; NFR-COST-01/03 deferral), [ADR-007](./ADR-007-summary-cache-key.md) (summary-cache 5-tuple — unchanged by this ADR).

## Summary

Three decisions:

1. **Clarion's `LlmProvider` implementations are OpenRouter (live HTTP) + two CLI bridges + a recording provider — not a native Anthropic provider.** The shipped set in `llm_provider.rs` is:
   - `RecordingProvider` — replay/testing (record-and-replay for deterministic tests).
   - `OpenRouterProvider` — the primary live provider; OpenAI-compatible chat-completions HTTP against OpenRouter. Anthropic models remain reachable as routed model strings (e.g. `anthropic/claude-sonnet-4.6`), but via OpenRouter, not a native Anthropic SDK.
   - `CodexCliProvider` — OpenAI Codex CLI subprocess bridge.
   - `ClaudeCliProvider` — Claude CLI subprocess bridge.
2. **The caching model is `OpenAiChatCompletions`, not Anthropic's explicit four-`cache_control`-breakpoint scheme.** All four providers return `CachingModel::OpenAiChatCompletions`. The four-segment *prompt* structure (system/project guidance → subsystem/module guidance → per-entity guidance → entity content) is retained as the prompt-assembly shape, but caching is provider-side OpenAI-style automatic prefix caching rather than caller-placed `cache_control` breakpoints.
3. **`CON-ANTHROPIC-01` is superseded.** The "Anthropic-only" constraint and its four-segment-caching premise no longer describe the system.

## Decision

Supersede `CON-ANTHROPIC-01`. The provider posture of record is: an OpenAI-compatible chat-completions abstraction with OpenRouter as the default live transport and CLI bridges for Codex / Claude, plus a recording provider for tests. Provider selection is configuration, not a hard-coded vendor.

## Consequences

**Positive**
- Provider portability is real, not aspirational: OpenRouter exposes many vendors (including Anthropic models) behind one OpenAI-compatible surface, so a model swap is configuration.
- CLI bridges let an operator drive Clarion through an already-authenticated local Codex/Claude CLI without managing a separate API key path.
- Testability is unchanged: `RecordingProvider` still backs deterministic replay (REQ-ANALYZE-07).

**Negative / tradeoff (the cost-caching point CON-ANTHROPIC-01 was protecting)**
- OpenAI-style automatic prefix caching does not give the caller the explicit, segment-boundary control Anthropic's `cache_control` breakpoints do. Cache-hit economics depend on the provider's prefix-cache behaviour and on prompt-prefix stability, not on four caller-placed breakpoints. The elspeth cost target (`NFR-COST-01`, already deferred to v1.1 per ADR-030) must be re-validated against the actual OpenRouter caching behaviour when the batched pipeline lands; it should not be assumed to match the Anthropic-breakpoint model the original constraint presumed.
- Routed Anthropic access via OpenRouter adds a hop (and OpenRouter's margin) versus a hypothetical native Anthropic provider.

## Follow-up

- `CON-ANTHROPIC-01` status flips to "Superseded by ADR-039" in `requirements.md`.
- `system-design.md` §5 (LLM provider abstraction, prompt caching) is reconciled to this ADR.
- The `NFR-COST-01` v1.1 re-validation must measure OpenRouter prefix-cache hit rates, not assume Anthropic-breakpoint savings.
