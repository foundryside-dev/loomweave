# OpenRouter LLM Provider

Clarion v0.1 uses OpenRouter as its canonical live LLM provider. The provider
speaks OpenAI-compatible Chat Completions HTTP through the existing
`LlmProvider` trait, so tests can continue to use `RecordingProvider` and future
providers can be added without changing MCP tool call sites.

## Configure Clarion

`clarion install` writes a default `clarion.yaml` with LLMs disabled. To enable
live OpenRouter calls, set a concrete model ID and opt in explicitly:

```yaml
llm_policy:
  enabled: true
  provider: openrouter
  allow_live_provider: true
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

The older `llm:` top-level key is accepted as a compatibility alias, but new
operator examples should use `llm_policy:`.

## API Key

Create an OpenRouter API key in the OpenRouter dashboard, then export the
environment variable named by `llm_policy.openrouter.api_key_env`:

```sh
export OPENROUTER_API_KEY=...
```

`clarion serve` does not construct a live provider unless LLMs are enabled and
`allow_live_provider: true` is set, or `CLARION_LLM_LIVE=1` is present. Missing
keys are reported as configuration errors instead of panicking at startup.

## Model IDs

OpenRouter model IDs are concrete strings such as:

- `anthropic/claude-sonnet-4.6`
- `openai/gpt-4o-mini`
- `meta-llama/llama-3.1-8b-instruct`

Clarion stores this exact string in the summary cache key's `model_tier`
component. There is no tier-name resolver in v0.1; operators choose the concrete
model in `clarion.yaml`. Refresh the chosen ID against OpenRouter's Models API
before release or production use.

## Token Ceiling

Clarion enforces a per-`clarion serve` session token ceiling, not a dollar
ceiling. Live responses debit OpenRouter's reported
`usage.total_tokens`; cache hits do not spend tokens. When the ceiling is
reached, MCP responses use:

- error code: `token-ceiling-exceeded`
- diagnostic: `CLA-LLM-TOKEN-CEILING-EXCEEDED`
- stat: `token_ceiling_exceeded_total`

Dollar budgeting remains an operator concern in OpenRouter billing controls.

## CI And Replay

CI must not call OpenRouter. Tests should use `RecordingProvider` fixtures or
pre-populated cache rows. Live smoke testing is manual and must require an
explicit API key plus live-provider opt-in.
