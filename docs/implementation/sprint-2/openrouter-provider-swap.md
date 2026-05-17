# OpenRouter Provider Swap

Status: Draft after source/API review
Date: 2026-05-18
Issue: `clarion-0009a39e0e`
Branch: `sprint-2/openrouter-swap`

## Purpose

This memo scopes the focused provider replacement that must land before B.8.
B.8 should measure the provider Clarion actually ships with in v0.1, so the
just-shipped `AnthropicProvider` is replaced by an `OpenRouterProvider`.

The `LlmProvider` trait remains. OpenRouter is the canonical v0.1 provider;
future Bedrock, native Anthropic, LiteLLM proxy, or other providers can be
added in v0.2+ without changing MCP call sites.

## S1. Wire Format

Clarion speaks OpenAI-compatible Chat Completions HTTP to OpenRouter.

- Base URL default: `https://openrouter.ai/api/v1`
- Chat endpoint: `POST {endpoint_url}/chat/completions`
- Direct default endpoint: `https://openrouter.ai/api/v1/chat/completions`
- Auth: `Authorization: Bearer <OPENROUTER_API_KEY>`
- Request body: `model`, `messages`, `temperature`, and a completion limit
  (`max_completion_tokens` preferred; `max_tokens` accepted/deprecated by
  OpenRouter), plus future OpenAI-compatible fields as needed.
- v0.1 message shape: one user message containing the rendered Clarion prompt.
- Response body: `choices[]` with a non-streaming `message`, plus `usage`.

The response parser consumes the first choice whose `message.content` is
non-empty. `usage.prompt_tokens`, `usage.completion_tokens`, and
`usage.total_tokens` are copied into Clarion's response/accounting path.
Streaming is out of scope for this swap.

Sources: OpenRouter quickstart and API reference describe `/api/v1/chat/completions`,
Bearer auth, OpenAI-compatible messages, non-streaming choices, and usage.

## S2. OpenRouter Attribution Headers

Clarion sends attribution headers on live OpenRouter calls:

- `HTTP-Referer`: default `https://github.com/qacona/clarion`
- `X-OpenRouter-Title`: default `Clarion`

OpenRouter documents `X-OpenRouter-Title` as the canonical app-title header and
still accepts `X-Title` for backwards compatibility. Clarion should emit the
canonical header.

Both values are configurable in `clarion.yaml`. They are not required for auth.
`HTTP-Referer` is required for app attribution/rankings; `X-OpenRouter-Title`
sets the display name when paired with `HTTP-Referer`, and `X-Title` is accepted
for backwards compatibility.

## S3. Model Naming

OpenRouter model IDs are the exact `id` strings returned by OpenRouter's Models
API and normally include an organization/provider prefix, such as:

- `anthropic/claude-sonnet-4.6`
- `openai/gpt-4o-mini`
- `meta-llama/llama-3.1-8b-instruct`

Examples are illustrative; defaults should be refreshed from `/api/v1/models`
before release.

Clarion does not implement tier-name to model-ID resolution in v0.1. Operators
choose the concrete `model_id` in config. ADR-007's `model_tier` cache-key
component stores that concrete string verbatim. The cache-key shape is unchanged;
only values move from native Anthropic model IDs to OpenRouter `vendor/model`
strings.

## S4. Pricing Math Removal

B.6 added Anthropic-specific pricing tables and dollar-budget estimates. Those
tables are removed.

Clarion records token counts from OpenRouter usage:

- prompt tokens
- completion tokens
- total tokens

Cost-per-token math stays outside Clarion. Operators already have OpenRouter's
billing dashboard and can choose models based on their own budget. Clarion's
enforcement becomes a token ceiling:

- Config: `llm.session_token_ceiling`
- Default: `1000000`
- Scope: one `clarion serve` process/session
- Cache hits do not spend tokens

Existing cache storage columns named `cost_usd` can remain for compatibility and
be written as `0.0` until a migration renames them. User-facing MCP stats should
report tokens, not calculated dollars.

## S5. Error Handling

OpenRouter standalone request errors use an envelope:

```json
{"error":{"code":401,"message":"Invalid credentials","metadata":{}}}
```

Provider errors may also appear inside a successful Chat Completions response
choice. The provider unwraps both shapes into `LlmProviderError`.

Classification:

- 4xx auth, insufficient credits, forbidden, moderation, model/request errors:
  non-retryable unless OpenRouter explicitly returns a retryable code.
- 429 and 503: OpenRouter may return a standard `Retry-After` header. Clarion
  v0.1 may choose not to sleep-and-retry inside a single MCP call, but the
  provider/MCP envelope must preserve retryability and retry-after context so
  callers can retry later.
- 5xx and connection/transport failures: retryable.
- Malformed JSON or schema mismatch is a Clarion parse/transport error;
  retryability is Clarion policy, not an OpenRouter-documented cause.

MCP envelopes should preserve retryability instead of always marking provider
errors retryable.

## S6. Config Surface

`clarion.yaml`:

```yaml
llm:
  enabled: true
  provider: openrouter
  allow_live_provider: false
  openrouter:
    endpoint_url: https://openrouter.ai/api/v1
    api_key_env: OPENROUTER_API_KEY
    attribution:
      referer: https://github.com/qacona/clarion
      title: Clarion
  model_id: anthropic/claude-sonnet-4.6
  session_token_ceiling: 1000000
  max_inferred_edges_per_caller: 8
  cache_max_age_days: 180
```

`endpoint_url` is configurable so a later LiteLLM proxy or local OpenAI-compatible
gateway can use `http://localhost:4000` without code changes. v0.1 defaults to
OpenRouter direct.

The old `anthropic_api_key_env` or `provider: anthropic` shape should produce a
clear `CLA-CONFIG-DEPRECATED-PROVIDER` configuration error/finding that points
operators to `provider: openrouter` and `llm.openrouter.api_key_env`.

`clarion serve` must not panic at startup with no API key. If live provider use
is not explicitly enabled, no provider is constructed. If live use is enabled
and the configured env var is missing, config validation reports the missing
OpenRouter key.

## S7. ADR Amendments

ADR-030 Decision 1 changes:

- `AnthropicProvider` -> `OpenRouterProvider`
- Consequence: v0.1 is OpenRouter-specific at the provider implementation layer,
  while the OpenAI-compatible wire format keeps later providers/proxies
  straightforward.

ADR-007 changes:

- No key-shape change.
- Add a note under `model_tier`: under OpenRouter the concrete model ID is the
  `vendor/model` string verbatim.

The accepted-ADR edit is narrow because the user explicitly requested an
amendment instead of a superseding ADR for this provider swap.

## S8. Panel Record

Reality reviewer: complete on 2026-05-18.

Review source of truth: OpenRouter documentation, especially quickstart, API
reference, authentication, errors/debugging, app attribution, and models.

Verdict: basic provider swap is real. Endpoint/base URL, Bearer auth,
`HTTP-Referer`, `X-OpenRouter-Title`, non-streaming `choices[].message`, and
`usage` are supported by current OpenRouter docs. The reviewer did not make a
live Chat Completions call with an API key.

Corrections folded from review:

- Use `X-OpenRouter-Title` as the emitted title header; `X-Title` is only the
  documented compatibility alias.
- Replace stale `anthropic/claude-3.5-sonnet` examples/defaults with current
  `anthropic/claude-sonnet-4.6` examples and note defaults should be refreshed
  from `/api/v1/models` before release.
- Preserve retryability and retry-after context for 429/503 instead of treating
  429 as permanently non-retryable.
- Prefer `max_completion_tokens`; `max_tokens` is accepted/deprecated.
- Remove unsupported free-tier attribution wording and undocumented parse-cause
  wording.
- Treat OpenRouter usage as token accounting and do not recompute provider
  pricing inside Clarion.

## Task Ledger

1. Memo plus one reality-review pass.
2. TDD provider replacement: happy path, error envelope, choice-level error,
   retryability, headers, and usage tokens.
3. Remove Anthropic pricing math and switch MCP ledger to token ceiling.
4. Update `clarion.yaml` parsing and deprecation diagnostics.
5. Wire `clarion serve` to construct `OpenRouterProvider`.
6. Keep `RecordingProvider` trait compatibility.
7. Amend ADR-030, ADR-007, and operator docs.
8. Extend the Sprint 2 MCP e2e with a RecordingProvider `summary()` smoke.

## Done Criteria

- Memo includes reality-review verdict.
- `OpenRouterProvider` replaces `AnthropicProvider`.
- New config shape parses; old Anthropic shape fails clearly.
- No live API calls in CI.
- Token counts surface through MCP summary/inferred stats and cache entries.
- ADR-023 gates pass.
- Branch is pushed, PR opened, and Filigree issue is closed.
