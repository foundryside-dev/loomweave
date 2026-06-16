# OpenRouter LLM Provider

Loomweave uses OpenRouter as its canonical HTTP LLM provider. The provider
speaks OpenAI-compatible Chat Completions HTTP through the existing
`LlmProvider` trait, so tests can continue to use `RecordingProvider` and
coding-agent CLI providers can be added without changing MCP tool call sites.

For local-login alternatives that avoid API keys in Loomweave config, see
[Coding-agent LLM providers](./coding-agent-llm-providers.md).

## Configure Loomweave

`loomweave install` writes a default `loomweave.yaml` with LLMs disabled. Print a
fresh annotated example any time with `loomweave config example` (or
`loomweave config example --provider claude_cli`), and after editing, run
`loomweave config check` to see the *effective* provider/live/model state and any
warnings (e.g. a provider configured but left `enabled: false`). To enable live
OpenRouter calls, set a concrete model ID and opt in explicitly:

```yaml
llm_policy:
  enabled: true
  provider: openrouter
  allow_live_provider: true
  openrouter:
    endpoint_url: https://openrouter.ai/api/v1
    api_key_env: OPENROUTER_API_KEY
    timeout_seconds: 300        # per-request HTTP timeout; must be > 0 (default 300)
    attribution:
      referer: https://github.com/foundryside-dev/loomweave
      title: Loomweave
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

`loomweave serve` does not construct a live provider unless LLMs are enabled and
`allow_live_provider: true` is set, or `LOOMWEAVE_LLM_LIVE=1` is present. Missing
keys are reported as configuration errors instead of panicking at startup.

## Model IDs

OpenRouter model IDs are concrete strings such as:

- `anthropic/claude-sonnet-4.6`
- `openai/gpt-4o-mini`
- `meta-llama/llama-3.1-8b-instruct`

Loomweave stores this exact string in the summary cache key's `model_tier`
component. There is no tier-name resolver in v1.0; operators choose the concrete
model in `loomweave.yaml`. Refresh the chosen ID against OpenRouter's Models API
before release or production use.

## Token Ceiling

Loomweave enforces a per-`loomweave serve` session token ceiling, not a dollar
ceiling. Live responses debit OpenRouter's reported
`usage.total_tokens`; cache hits do not spend tokens. When the ceiling is
reached, MCP responses use:

- error code: `token-ceiling-exceeded`
- diagnostic: `LMWV-LLM-TOKEN-CEILING-EXCEEDED`
- stat: `token_ceiling_exceeded_total`

The ceiling is scoped to the running `loomweave serve` process. Once a live LLM
call attempts to exceed `llm_policy.session_token_ceiling`, Loomweave blocks new
cold LLM dispatches for the rest of that process lifetime. Cache hits can still
be returned while the budget is blocked, because they do not spend additional
tokens.

To clear a blocked LLM budget, stop and restart `loomweave serve`. To change the
future ceiling, edit `llm_policy.session_token_ceiling` in `loomweave.yaml` before
restarting. Loomweave v1.0 intentionally has no MCP tool that resets the in-memory
budget ledger.

Dollar budgeting remains an operator concern in OpenRouter billing controls.

## CI And Replay

CI must not call OpenRouter. Tests should use `RecordingProvider` fixtures or
pre-populated cache rows. Live smoke testing is manual and must require an
explicit API key plus live-provider opt-in.
