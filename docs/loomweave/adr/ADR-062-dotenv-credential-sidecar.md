# ADR-062: Repository `.env` Is a Credential Sidecar, Never Environment

**Status**: Accepted
**Date**: 2026-09-02
**Deciders**: john@foundryside.dev
**Context**: Review of the Codex security PRs #142–#151 for the 1.6.1 patch. #143 (landed as #149) showed the session-start hook handing a loaded repository `.env` to the `analyze` child it spawns. Auditing the same class found that `serve` — launched automatically by agent harnesses on any checkout the operator opens — loads the same file and then (a) spawns `loomweave analyze` (`analyze_start`), `loomweave worktree analyze`, `git`, and the Filigree / Warpline MCP launchers with that environment, and (b) reads `LOOMWEAVE_FILIGREE_MCP_COMMAND` / `LOOMWEAVE_WARPLINE_MCP_COMMAND` from it to decide *which program to execute*. `dotenvy` sets only variables that are unset, so the normally-unset ones — those launcher overrides, `LD_PRELOAD`, `PYTHONPATH`, `HTTPS_PROXY` — were exactly the attacker-fillable set. A committed `.env` chose the program `serve` ran.

## Summary

`.env` is no longer loaded into the process environment by any command. It is parsed once into a private, process-global map (`loomweave_core::dotenv::load_sidecar`) and consulted **only** by `loomweave_core::dotenv::var`, which every config-named credential lookup now calls (provider `api_key_env`, Filigree `token_env` / `identity_token_env`, `RUST_LOG`). The real environment always wins over the sidecar. Nothing else changes: `std::env::var` reads — every launcher and path override — cannot observe `.env`, and no child process inherits it, by construction rather than by per-site scrubbing.

## Context

- `crates/loomweave-cli/src/main.rs` called `dotenvy::dotenv()` for every command except `analyze`, `worktree analyze`, `hook`, and the editor-spawning `guidance` subcommands. Those exclusions were sound but pointwise: each new spawn site or env-read override reopened the hole (`serve` had three spawn sites and two override reads).
- The legitimate operator use of a repository `.env` is narrow: values for variables Loomweave's own configuration *names*. Documentation already tells operators to `export` provider keys or put them in the MCP server's `env` block; `.env` was a convenience, and remains one for exactly those lookups.
- The workspace denies `unsafe_code`; `std::env::remove_var` is `unsafe` in edition 2024, so "load then scrub the process environment" was not available either. Command-level scrubbing (`Command::env_remove` of recorded keys at every spawn site) would have needed a cross-crate registry and would still have left in-process override reads exposed.

## Decision

1. **Sidecar, not environment.** `loomweave_core::dotenv::load_sidecar()` parses the file `dotenvy::dotenv()` would have loaded (cwd, then ancestors) into a `OnceLock<HashMap>`; it never calls `set_var`. `var(name)` returns the process value if set, else the sidecar value, else `None`.
2. **One reader.** Every closure of the form `|name| std::env::var(name).ok()` handed to `select_provider_with_env`, `build_embedding_provider`, `resolve_filigree_url_with_roots`, `validate_auth_trust`, and the direct `api_key_env` / `token_env` reads in `serve`, `doctor`, `config`, `guidance`, `sarif`, `http_read`, and the MCP server, now calls `loomweave_core::dotenv::var`. `init_tracing` builds its filter from `var("RUST_LOG")` instead of `EnvFilter::try_from_default_env`, so a `.env`-supplied `RUST_LOG` keeps working.
3. **Overrides stay on `std::env::var`.** `LOOMWEAVE_FILIGREE_MCP_COMMAND`, `LOOMWEAVE_WARPLINE_MCP_COMMAND`, `LOOMWEAVE_CODEX_CONFIG`, `LOOMWEAVE_PYTHON_INTERPRETER`, the plugin timeout knobs, `VISUAL` / `EDITOR`, and `PATH` are deliberately *not* routed through the sidecar. A launcher override must come from the operator's real environment.
4. **The command exclusion list survives** as defence in depth (`analyze` before the secret-scan gate, `hook`, editor-spawning `guidance`), but its rationale is now "must not read repository credentials at all", not "must not leak them to children".

## Consequences

### Positive

- The whole class closes at once: `serve`'s `analyze_start` child, the linked-worktree bootstrap child, the Filigree / Warpline MCP launchers, `git`, and any future spawn site inherit nothing from `.env`; no `std::env::var` override can be supplied by a checkout.
- Pinned by a core test that writes a `.env` naming `LOOMWEAVE_FILIGREE_MCP_COMMAND` and `LD_PRELOAD`, loads the sidecar, and asserts the values are visible to `var` but absent from `std::env::var_os` and from a spawned `sh` child; the existing CLI integration tests for `.env`-supplied `RUST_LOG` and never-clobber precedence keep passing unchanged.

### Negative

- Anything a third-party library read from a `.env`-supplied variable stops working: `HTTPS_PROXY` / `HTTP_PROXY` for `reqwest`, `SSL_CERT_FILE`, `NO_PROXY`. Operators who relied on that must export those in their shell or the MCP server `env` block. Recorded in the 1.6.1 changelog.
- A plugin or sibling that previously (unintentionally) received a `.env` value through `serve`'s environment no longer does; that was the vulnerability.

## Related Decisions

- **Builds on**: [ADR-013](./ADR-013-pre-ingest-secret-scanner.md) (`.env` is scanned as an untrusted source sidecar), [ADR-045](./ADR-045-worktree-source-staleness.md) (untrusted-corpus posture).
- **Related to**: [ADR-058](./ADR-058-project-interpreter-discovery.md) (`LOOMWEAVE_PYTHON_INTERPRETER` stays a real-environment override), [ADR-061](./ADR-061-plugin-process-tree-and-serve-parent-liveness.md) (`serve` as a long-lived, harness-launched process). Follow-ups on the same trust boundary: clarion-9b3cf287b7 (repository-tracked `.venv`), clarion-dee44f1a66 (repository-tracked `loomweave.yaml` directing egress).
