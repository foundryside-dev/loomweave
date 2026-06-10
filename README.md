# Loomweave

Loomweave is a code-archaeology tool. It ingests a codebase, extracts entities
(functions, classes, modules) and their relationships (`contains`, `calls`,
`references`), persists the structural graph to a local SQLite store, and serves
the result to consult-mode LLM agents over MCP. A coding agent that would
otherwise re-explore the tree on every question reaches Loomweave first and asks a
graph-aware tool. The current release line ships a Rust core plus a Python
language plugin; other languages remain future scope.

Part of the [Weft suite](docs/suite/weft.md) of code-archaeology, issue-tracking,
and trust-topology tools.

## Status

**v1.0.0 — current release line.** Scope:

- **Python only.** Other-language plugins (`NG-15`) are v2.0+ scope.
- **Structural extraction + on-demand LLM summarisation.** `loomweave analyze`
  walks the corpus and persists entities + edges; `summary(id)` over MCP
  dispatches the LLM lazily, one entity at a time.
- **Local-first.** No mandatory cloud component; the only required network
  egress is the LLM provider during `summary` calls.
- **Stable identity and suite enrichment.** Loomweave mints Stable Entity
  Identity (SEI) tokens, serves the federation HTTP read API, emits opted-in
  Filigree scan findings (issue lookups now key by SEI), and enriches MCP reads
  with Filigree/Wardline context without making sibling products mandatory.
- **Wardline trust vocabulary via on-disk descriptor.** The Python plugin reads
  Wardline's NG-25 trust-vocabulary descriptor as a plain file and tags
  trust-decorated entities (`wardline:*`) — without importing Wardline, so a
  co-installed Wardline is not required. Degrades cleanly when the descriptor is
  absent. (Retires the last Loomweave-side federation asterisk; see
  [`docs/suite/weft.md`](docs/suite/weft.md) §5.)
- **Guidance authoring.** Operators can author, import, export, and review
  guidance sheets through `loomweave guidance`; consult agents consume them
  through MCP and summary cache invalidation.

**Known limitations:**

- **HTTP file language inference uses stored plugin identity plus a narrow
  core-extension fallback.** Plugin manifests declare language and extensions,
  but Loomweave does not yet persist a manifest language registry for the
  `/api/v1/files` read path.
- **Some guidance lifecycle surfaces remain deferred.** The in-browser
  staleness-review UI is still tracked separately; authored guidance is
  available through the CLI and MCP read path today.

## What it does today

`loomweave serve` exposes a ~42-tool MCP surface that a consult-mode agent calls
instead of grep-and-read. The core tool families are:

| Family | Examples |
|---|---|
| Navigation and graph traversal | `entity_at`, `entity_find`, `entity_callers_list`, `entity_execution_path_list`, `entity_neighborhood_get`, `subsystem_member_list`, `entity_call_site_list` |
| Briefing and source inspection | `entity_summary_get`, `entity_summary_preview_cost_get`, `entity_source_get`, `entity_orientation_pack_get`, `project_status_get` |
| Guidance, findings, and federation context | `entity_guidance_list`, `propose_guidance`, `promote_guidance`, `entity_finding_list`, `entity_wardline_get`, `entity_issue_list` |
| Analyze lifecycle and freshness | `analyze_start`, `analyze_status_get`, `analyze_cancel`, `index_diff_get` |
| Faceted and shortcut queries | `entity_tag_list`, `entity_kind_list`, `module_circular_import_list`, `entity_coupling_hotspot_list`, `entity_entry_point_list`, `entity_dead_list`, `entity_semantic_search_list` |

## Quick start

```bash
# 1. Install from the current GitHub Release
TAG=v1.0.0
curl -L -o loomweave-x86_64-unknown-linux-gnu.tar.gz \
  "https://github.com/foundryside-dev/loomweave/releases/download/${TAG}/loomweave-x86_64-unknown-linux-gnu.tar.gz"
tar xzf loomweave-x86_64-unknown-linux-gnu.tar.gz
install loomweave-x86_64-unknown-linux-gnu/loomweave ~/.local/bin/
pipx install \
  "https://github.com/foundryside-dev/loomweave/releases/download/${TAG}/loomweave-plugin-python-1.0.0.tar.gz"

# 2. Initialise a project
cd /path/to/your/python/repo
loomweave install --path .

# 3. Walk the corpus and persist the structural graph
loomweave analyze

# 4. Serve the graph over MCP for consult-mode agents
loomweave serve
```

`loomweave install` is the one-step agent setup path: it initialises `.weft/loomweave/`,
installs the `loomweave-workflow` skill for Claude Code and Codex, writes Claude
Code MCP config, upserts Codex MCP config, and installs the SessionStart hook.
Use component flags such as `--claude-code`, `--codex`, `--skills`,
`--codex-skills`, and `--hooks` for partial installs.

`loomweave analyze` works without any LLM credentials and is the fastest way to
verify the install. `summary(id)` calls require `OPENROUTER_API_KEY` to be set
(see [docs/operator/openrouter.md](docs/operator/openrouter.md)).

A full walkthrough — installing on a fresh machine, running against a small
public Python project, connecting an MCP client, and asking three questions — is
in [docs/operator/getting-started.md](docs/operator/getting-started.md).

## Project layout

```
crates/                 Rust workspace
├── loomweave-core/       Entity-ID assembler, plugin host, manifest parser
├── loomweave-storage/    Writer-actor + reader-pool over SQLite (ADR-011)
├── loomweave-federation/ Shared federation HTTP types
├── loomweave-scanner/    Pre-ingest secret scanner (ADR-013, WP5)
├── loomweave-cli/        The `loomweave` binary (install, analyze, serve)
└── loomweave-mcp/        MCP server exposing the consult tools
plugins/python/         Python language plugin (pyright-backed)
docs/loomweave/1.0/      Design ladder — requirements → system-design → detailed-design
docs/loomweave/adr/       Authored architecture decision records
```

For the design ladder start at
[docs/loomweave/1.0/README.md](docs/loomweave/1.0/README.md). The full ADR index
is at [docs/loomweave/adr/README.md](docs/loomweave/adr/README.md). The Weft
federation doctrine that anchors every cross-product decision is in
[docs/suite/weft.md](docs/suite/weft.md).

## Storage and operations

Loomweave keeps project state in a local `.weft/loomweave/` directory.
The local-first storage model, the no-NFS constraint, the no-double-analyze
constraint (fs2 advisory lock), and the backup/restore procedure are
documented in
[docs/loomweave/1.0/operations.md](docs/loomweave/1.0/operations.md).

## Contributing

Read the [v1.0 docset README](docs/loomweave/1.0/README.md) for the canonical
design ladder, its reading order, and where canonical truth lives. The CI
floor every PR must clear is fixed by
[ADR-023](docs/loomweave/adr/ADR-023-tooling-baseline.md):

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
bash tests/e2e/sprint_2_mcp_surface.sh
bash tests/e2e/phase3_subsystems.sh
```

Pre-commit hooks at [.pre-commit-config.yaml](.pre-commit-config.yaml) wire
ruff + ruff-format + mypy on every `git commit`. Install with
`plugins/python/.venv/bin/pre-commit install`.

## License

[MIT](LICENSE). Matches the `license = "MIT"` declaration in
[`Cargo.toml`](Cargo.toml). Contributions are accepted under the same terms
unless explicitly stated otherwise by the contributor.
