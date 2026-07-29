# Operator Notes

Practical notes for configuring and running Loomweave.

**Supported platforms:** Linux is the supported and CI-verified target
(macOS additionally gets a CI build check on `aarch64-apple-darwin`).
Windows is **not** a supported target — the plugin host's process
sandboxing (`setrlimit`) and the worktree confined-deletion primitive
(`openat2`) are Unix/Linux mechanisms, and the workspace does not build for
Windows (owner ruling 2026-07-29, clarion-614022d526).

- [Getting started](./getting-started.md) — single-flow walkthrough: install,
  analyse a small repo, connect an MCP client, ask three questions, verify
  the secret-block. Target ≤15 minutes end-to-end.
- [Language support](./language-support.md) — what each language plugin (Python,
  Rust) extracts and tags, side by side: entity/edge kinds, categorisation tags,
  and which tools work per language. The two plugins do not cover the same
  surface, so check here before reading a per-language result as complete.
- [Rust analysis: known limitations](./rust-known-limitations.md) — what Rust
  analysis does and does not resolve (macros, external edges, dead-code roots).
- [OpenRouter LLM provider](./openrouter.md) — API key, model ID, attribution
  headers, and token-ceiling configuration.
- [Coding-agent LLM providers](./coding-agent-llm-providers.md) — Codex CLI
  and Claude CLI as local-login alternatives to API-key provider wiring.
- [Runtime topology](./runtime-topology.md) — supported `loomweave serve` and
  `loomweave analyze` concurrency against one `.weft/loomweave/loomweave.db`.
- [Secret scanning](./secret-scanning.md) — pre-ingest scanner behavior,
  baseline false-positive workflow, override confirmation, and audit queries.
- [Guidance](./guidance.md) — authoring guidance sheets with the `loomweave
  guidance` CLI, `--match`/`--scope-level`/`--expires` semantics, staleness
  findings, and the export/import team-sharing workflow.
- [Release handoff](./release-handoff.md) — retired
  Loomweave-owned GitHub ruleset enforcement and current standalone release
  sequence.
- [Federation contracts](../federation/contracts.md) — read-side HTTP
  contracts consumed by sibling products such as Filigree.
