# Operator Notes

Practical notes for configuring and running Clarion.

- [Getting started](./getting-started.md) — single-flow walkthrough: install,
  analyse a small repo, connect an MCP client, ask three questions, verify
  the secret-block. Target ≤15 minutes end-to-end.
- [OpenRouter LLM provider](./openrouter.md) — API key, model ID, attribution
  headers, and token-ceiling configuration.
- [Coding-agent LLM providers](./coding-agent-llm-providers.md) — Codex CLI
  and Claude CLI as local-login alternatives to API-key provider wiring.
- [Runtime topology](./runtime-topology.md) — supported `clarion serve` and
  `clarion analyze` concurrency against one `.clarion/clarion.db`.
- [Secret scanning](./secret-scanning.md) — pre-ingest scanner behavior,
  baseline false-positive workflow, override confirmation, and audit queries.
- [Guidance](./guidance.md) — authoring guidance sheets with the `clarion
  guidance` CLI, `--match`/`--scope-level`/`--expires` semantics, staleness
  findings, and the export/import team-sharing workflow.
- [v1.0 release governance](./v1.0-release-governance.md) — maintainer steps
  for GitHub branch/ruleset enforcement, Actions policy, release dry run, and
  final tag gating.
- [Federation contracts](../federation/contracts.md) — read-side HTTP
  contracts consumed by sibling products such as Filigree.
