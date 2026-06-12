# Coding-Agent LLM Providers

Loomweave can run live summary and inferred-edge LLM calls through local coding
agent CLIs instead of an HTTP API key. These routes still send prompt content to
the agent vendor, so they require the same explicit live-provider opt-in as
OpenRouter.

Supported provider values:

- `codex_cli` uses `codex exec` with local Codex authentication.
- `claude_cli` uses `claude -p` with local Claude Code authentication.
- `openrouter` remains the HTTP provider for API-key based deployments.
- `recording` remains the deterministic test fixture provider.

Operator-facing aliases are also accepted: `codex_sidecar` maps to
`codex_cli`, `claude_sidecar` maps to `claude_cli`, and `openrouter_api` maps to
`openrouter`. `loomweave config example --provider <alias>` emits the canonical
provider value in the generated YAML.

## Configure From CLI Or MCP

Prefer the config helpers over hand-editing `loomweave.yaml`:

```sh
loomweave config llm set \
  --enable \
  --allow-live \
  --provider codex_sidecar \
  --enable-write-tools
```

Use `loomweave config llm status` or `loomweave config check` to inspect the
effective state. The same bootstrap surface is available over MCP:
`llm_config_get` reads the current config and `llm_config_set` updates
`loomweave.yaml` fields such as `enabled`, `provider`, `allow_live_provider`,
and `enable_write_tools`. Reconnect or restart `loomweave serve` after changing
provider or write-tool policy, because the active server loads those settings at
startup.

## Codex CLI

```yaml
llm_policy:
  enabled: true
  provider: codex_cli
  allow_live_provider: true
  model_id: codex-cli-default
  codex_cli:
    executable: codex
    model: null
    profile: null
    sandbox: read-only
    timeout_seconds: 300
  session_token_ceiling: 1000000
  max_inferred_edges_per_caller: 8
  cache_max_age_days: 180
```

Run `codex login status` before `loomweave serve`. Loomweave passes prompts on
stdin, uses `--sandbox read-only`, `approval_policy="never"`, `--json`,
`--output-last-message`, and `--output-schema`. Token usage is read from Codex
JSONL events when available; otherwise Loomweave falls back to local estimates.

Set `codex_cli.model` only when you want to force a specific Codex model. When
it is `null`, the Codex CLI profile/default model is used. `model_id` is the
Loomweave cache-key label for this route, so change it deliberately when changing
the effective model or prompt-routing behavior.

## Claude CLI

```yaml
llm_policy:
  enabled: true
  provider: claude_cli
  allow_live_provider: true
  model_id: claude-code-default
  claude_cli:
    executable: claude
    model: null
    permission_mode: plan
    tools: []
    timeout_seconds: 300
    max_turns: 2
    no_session_persistence: true
    exclude_dynamic_system_prompt_sections: true
  session_token_ceiling: 1000000
  max_inferred_edges_per_caller: 8
  cache_max_age_days: 180
```

Run `claude auth status` before `loomweave serve`. Loomweave uses print mode
(`-p`), `--output-format json`, `--json-schema`, and stdin prompts. The default
tool list is empty because Loomweave already supplies the source excerpt and
candidate graph context; add read-only tools only when you intentionally want
Claude Code to inspect the workspace. Loomweave also passes an explicit empty MCP
config, `--strict-mcp-config`, and `--disable-slash-commands` so project/user
MCP servers and skills do not inflate this bounded provider call.

`exclude_dynamic_system_prompt_sections: true` is enabled by default because
Claude Code documents it as a prompt-cache reuse aid for scripted workloads.
`max_turns: 2` is intentional: Claude Code's structured-output path consumes one
internal tool turn and then emits the final JSON result.

## Live Opt-In

`llm_policy.enabled: true` is not enough for any live provider. Set
`allow_live_provider: true` with `loomweave config llm set --enable --allow-live`
or in `loomweave.yaml`, or launch with:

```sh
LOOMWEAVE_LLM_LIVE=1 loomweave serve --path .
```

Unlike OpenRouter, the CLI routes do not require `OPENROUTER_API_KEY` or an
OpenAI/Anthropic API key in Loomweave config. They rely on the local CLI's own
login state.

## Lookup Traffic Log

Every configured LLM lookup appends one JSONL metadata record to
`.loomweave/diagnostics/llm-traffic.jsonl`. The log records the provider,
purpose, prompt template ID, model/cache label, outcome, token usage, and cost
when available. It deliberately does not record the prompt text or the model
output JSON. The diagnostics log is capped at 10 MiB; when it reaches the cap,
Loomweave rotates it to `llm-traffic.jsonl.1` before writing the next lookup.

## Prompt Caching And Advanced Interfaces

Loomweave already has its own semantic result cache for summaries and inferred
edges. Provider prompt caching is separate: it depends on the vendor surface,
stable prompt prefixes, model choice, and usage telemetry.

The CLI routes preserve stable invocation shape and expose cached-input-token
usage when the CLI reports it. For deeper OpenAI integration, the Codex
app-server protocol is the likely next provider shape: it is a JSON-RPC
interface for authentication, conversation history, approvals, and streamed
agent events. The OpenAI docs recommend the Codex SDK for automation/CI and the
app-server protocol for rich product integrations.

## Shared Agent Prompt Contract

Both CLI providers wrap Loomweave's leaf-summary and inferred-edge prompts in the
same stable provider prompt before invoking the local agent. Future SDK or
app-server providers should use `build_coding_agent_provider_prompt()` from
`loomweave_core` so all coding-agent surfaces share the same behavior:

```text
Prompt contract: loomweave-agent-provider-v1
You are Loomweave's coding-agent LLM provider for repository graph enrichment.
Loomweave has already selected the source excerpt, entity metadata, unresolved
call sites, and candidate graph context needed for this task.
Follow these rules exactly:
1. Use only the evidence inside <loomweave_request>. Do not inspect additional
   files, browse, run commands, edit files, or ask follow-up questions.
2. Return exactly one JSON object matching the structured-output schema
   supplied by the caller. Do not wrap it in Markdown or prose.
3. Reason privately if needed, but do not expose hidden reasoning. Put only
   concise evidence summaries in output fields that ask for rationale or
   relationships.
4. When evidence is absent, prefer empty strings for optional prose fields and
   empty arrays for collection fields instead of guessing.
5. Keep stable field names and JSON types; downstream Loomweave storage parses the
   response mechanically.
```

SDK providers should pass the same prompt as the user/task content and attach
Loomweave's purpose-specific JSON Schema through the SDK's structured-output
mechanism. Do not replace this with an agentic "go inspect the repo" prompt:
Loomweave's MCP path is a bounded graph-enrichment call, not an autonomous code
review session.

Prompt surfaces by provider:

- Codex CLI: pass the shared prompt to `codex exec -` on stdin and attach
  Loomweave's JSON Schema with `--output-schema`.
- Claude CLI: use the short print-mode bootstrap prompt, pass the shared prompt
  on stdin, and attach Loomweave's JSON Schema with `--json-schema`.
- Codex SDK or app-server: put the same shared prompt in the task/user content,
  keep the developer/system instruction to "Loomweave graph enrichment, JSON only,
  no repository inspection", and attach the same purpose-specific schema through
  structured output.
- Claude Agent SDK: use the same task prompt and schema, disable tools by
  default, and grant only read-only tools when an operator explicitly wants
  workspace inspection.

The stable prefix is intentional. It keeps provider prompt-caching viable while
the volatile source excerpt, unresolved call sites, and candidates stay inside
the `<loomweave_request>` block.

Do not use either CLI route as the pre-ingest secret scanner. Loomweave's local
secret scanner must continue to run before any prompt content is sent to a live
provider.
