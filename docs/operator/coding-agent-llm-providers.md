# Coding-Agent LLM Providers

Clarion can run live summary and inferred-edge LLM calls through local coding
agent CLIs instead of an HTTP API key. These routes still send prompt content to
the agent vendor, so they require the same explicit live-provider opt-in as
OpenRouter.

Supported provider values:

- `codex_cli` uses `codex exec` with local Codex authentication.
- `claude_cli` uses `claude -p` with local Claude Code authentication.
- `openrouter` remains the HTTP provider for API-key based deployments.
- `recording` remains the deterministic test fixture provider.

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

Run `codex login status` before `clarion serve`. Clarion passes prompts on
stdin, uses `--sandbox read-only`, `approval_policy="never"`, `--json`,
`--output-last-message`, and `--output-schema`. Token usage is read from Codex
JSONL events when available; otherwise Clarion falls back to local estimates.

Set `codex_cli.model` only when you want to force a specific Codex model. When
it is `null`, the Codex CLI profile/default model is used. `model_id` is the
Clarion cache-key label for this route, so change it deliberately when changing
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

Run `claude auth status` before `clarion serve`. Clarion uses print mode
(`-p`), `--output-format json`, `--json-schema`, and stdin prompts. The default
tool list is empty because Clarion already supplies the source excerpt and
candidate graph context; add read-only tools only when you intentionally want
Claude Code to inspect the workspace. Clarion also passes an explicit empty MCP
config, `--strict-mcp-config`, and `--disable-slash-commands` so project/user
MCP servers and skills do not inflate this bounded provider call.

`exclude_dynamic_system_prompt_sections: true` is enabled by default because
Claude Code documents it as a prompt-cache reuse aid for scripted workloads.
`max_turns: 2` is intentional: Claude Code's structured-output path consumes one
internal tool turn and then emits the final JSON result.

## Live Opt-In

`llm_policy.enabled: true` is not enough for any live provider. Set
`allow_live_provider: true` in `clarion.yaml`, or launch with:

```sh
CLARION_LLM_LIVE=1 clarion serve --path .
```

Unlike OpenRouter, the CLI routes do not require `OPENROUTER_API_KEY` or an
OpenAI/Anthropic API key in Clarion config. They rely on the local CLI's own
login state.

## Prompt Caching And Advanced Interfaces

Clarion already has its own semantic result cache for summaries and inferred
edges. Provider prompt caching is separate: it depends on the vendor surface,
stable prompt prefixes, model choice, and usage telemetry.

The CLI routes preserve stable invocation shape and expose cached-input-token
usage when the CLI reports it. For deeper OpenAI integration, the Codex
app-server protocol is the likely next provider shape: it is a JSON-RPC
interface for authentication, conversation history, approvals, and streamed
agent events. The OpenAI docs recommend the Codex SDK for automation/CI and the
app-server protocol for rich product integrations.

## Shared Agent Prompt Contract

Both CLI providers wrap Clarion's leaf-summary and inferred-edge prompts in the
same stable provider prompt before invoking the local agent. Future SDK or
app-server providers should use `build_coding_agent_provider_prompt()` from
`clarion_core` so all coding-agent surfaces share the same behavior:

```text
Prompt contract: clarion-agent-provider-v1
You are Clarion's coding-agent LLM provider for repository graph enrichment.
Clarion has already selected the source excerpt, entity metadata, unresolved
call sites, and candidate graph context needed for this task.
Follow these rules exactly:
1. Use only the evidence inside <clarion_request>. Do not inspect additional
   files, browse, run commands, edit files, or ask follow-up questions.
2. Return exactly one JSON object matching the structured-output schema
   supplied by the caller. Do not wrap it in Markdown or prose.
3. Reason privately if needed, but do not expose hidden reasoning. Put only
   concise evidence summaries in output fields that ask for rationale or
   relationships.
4. When evidence is absent, prefer empty strings for optional prose fields and
   empty arrays for collection fields instead of guessing.
5. Keep stable field names and JSON types; downstream Clarion storage parses the
   response mechanically.
```

SDK providers should pass the same prompt as the user/task content and attach
Clarion's purpose-specific JSON Schema through the SDK's structured-output
mechanism. Do not replace this with an agentic "go inspect the repo" prompt:
Clarion's MCP path is a bounded graph-enrichment call, not an autonomous code
review session.

Prompt surfaces by provider:

- Codex CLI: pass the shared prompt to `codex exec -` on stdin and attach
  Clarion's JSON Schema with `--output-schema`.
- Claude CLI: use the short print-mode bootstrap prompt, pass the shared prompt
  on stdin, and attach Clarion's JSON Schema with `--json-schema`.
- Codex SDK or app-server: put the same shared prompt in the task/user content,
  keep the developer/system instruction to "Clarion graph enrichment, JSON only,
  no repository inspection", and attach the same purpose-specific schema through
  structured output.
- Claude Agent SDK: use the same task prompt and schema, disable tools by
  default, and grant only read-only tools when an operator explicitly wants
  workspace inspection.

The stable prefix is intentional. It keeps provider prompt-caching viable while
the volatile source excerpt, unresolved call sites, and candidates stay inside
the `<clarion_request>` block.

Do not use either CLI route as the pre-ingest secret scanner. Clarion's local
secret scanner must continue to run before any prompt content is sent to a live
provider.
