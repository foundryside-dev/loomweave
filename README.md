# Clarion

Clarion is a code-archaeology tool. It ingests a codebase, extracts entities
(functions, classes, modules) and their relationships (`contains`, `calls`,
`references`), persists the structural graph to a local SQLite store, and serves
the result to consult-mode LLM agents over MCP. A coding agent that would
otherwise re-explore the tree on every question reaches Clarion first and asks a
graph-aware tool. v1.0 ships a Rust core plus a Python language plugin; other
languages land in v2.0+.

Part of the [Loom suite](docs/suite/loom.md) of code-archaeology, issue-tracking,
and trust-topology tools.

## Status

**v1.0 — first publishable release.** Scope:

- **Python only.** Other-language plugins (`NG-15`) are v2.0+ scope.
- **Structural extraction + on-demand LLM summarisation.** `clarion analyze`
  walks the corpus and persists entities + edges; `summary(id)` over MCP
  dispatches the LLM lazily, one entity at a time.
- **Local-first.** No mandatory cloud component; the only required network
  egress is the LLM provider during `summary` calls.
- **Filigree finding emission deferred to v0.2.** Clarion v1.0 surfaces issues
  attached to entities via its own `issues_for` MCP tool (the WP9-A binding);
  cross-product POSTing of Clarion-generated findings into Filigree's intake is
  WP9-B, deferred per the
  [Sprint 2 scope amendment](docs/implementation/sprint-2/scope-amendment-2026-05.md#4-v01-planmd-resequencing).

## What it does today

`clarion serve` exposes seven MCP tools that a consult-mode agent calls instead
of grep-and-read:

| Tool | What it answers |
|---|---|
| `entity_at(file, line)` | "Which entity covers this source location?" |
| `find_entity(pattern)` | "Find entities whose name or summary text matches X." |
| `callers_of(id)` | "Who calls this function?" |
| `execution_paths_from(id, max_depth)` | "Show me up to N hops of call paths starting here." |
| `summary(id)` | "Give me a one-paragraph summary of this entity." (lazy LLM dispatch + cached) |
| `issues_for(id)` | "What Filigree issues are attached to this entity?" |
| `neighborhood(id)` | "Show callers, callees, container, contained entities, and references in one hop." |

`subsystem_members(id)` is also exposed for listing members of a subsystem
entity (clustering output).

## Quick start

```bash
# 1. Install (see docs/operator/getting-started.md for the full setup)
cargo install --git https://github.com/tachyon-beep/clarion clarion-cli
pipx install git+https://github.com/tachyon-beep/clarion#subdirectory=plugins/python

# 2. Initialise a project
cd /path/to/your/python/repo
clarion install --path .

# 3. Walk the corpus and persist the structural graph
clarion analyze

# 4. Serve the graph over MCP for consult-mode agents
clarion serve
```

`clarion analyze` works without any LLM credentials and is the fastest way to
verify the install. `summary(id)` calls require `OPENROUTER_API_KEY` to be set
(see [docs/operator/openrouter.md](docs/operator/openrouter.md)).

A full walkthrough — installing on a fresh machine, running against a small
public Python project, connecting an MCP client, and asking three questions — is
in [docs/operator/getting-started.md](docs/operator/getting-started.md).

## Project layout

```
crates/                 Rust workspace
├── clarion-core/       Entity-ID assembler, plugin host, manifest parser
├── clarion-storage/    Writer-actor + reader-pool over SQLite (ADR-011)
├── clarion-scanner/    Pre-ingest secret scanner (ADR-013, WP5)
├── clarion-cli/        The `clarion` binary (install, analyze, serve)
└── clarion-mcp/        MCP server exposing the seven consult tools
plugins/python/         Python language plugin (pyright-backed)
docs/clarion/v0.1/      Design ladder — requirements → system-design → detailed-design
docs/clarion/adr/       Authored architecture decision records
```

For the design ladder start at
[docs/clarion/v0.1/README.md](docs/clarion/v0.1/README.md). The full ADR index
is at [docs/clarion/adr/README.md](docs/clarion/adr/README.md). The Loom
federation doctrine that anchors every cross-product decision is in
[docs/suite/loom.md](docs/suite/loom.md).

## Contributing

Read [CLAUDE.md](CLAUDE.md) for repository conventions, work-package
vocabulary, and where canonical truth lives. The CI floor every PR must clear
is fixed by [ADR-023](docs/clarion/adr/ADR-023-tooling-baseline.md):

```bash
# Rust gates
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check

# Python gates (run from repo root)
plugins/python/.venv/bin/ruff check plugins/python
plugins/python/.venv/bin/ruff format --check plugins/python
plugins/python/.venv/bin/mypy --strict plugins/python
plugins/python/.venv/bin/pytest plugins/python

# End-to-end
bash tests/e2e/sprint_1_walking_skeleton.sh
```

Pre-commit hooks at [.pre-commit-config.yaml](.pre-commit-config.yaml) wire
ruff + ruff-format + mypy on every `git commit`. Install with
`plugins/python/.venv/bin/pre-commit install`.

## License

[MIT](LICENSE). Matches the `license = "MIT"` declaration in
[`Cargo.toml`](Cargo.toml). Contributions are accepted under the same terms
unless explicitly stated otherwise by the contributor.
