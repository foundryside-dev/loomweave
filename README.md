# Clarion

Clarion is a code-archaeology tool. It ingests a codebase, extracts entities
(functions, classes, modules) and their relationships (`contains`, `calls`,
`references`), persists the structural graph to a local SQLite store, and serves
the result to consult-mode LLM agents over MCP. A coding agent that would
otherwise re-explore the tree on every question reaches Clarion first and asks a
graph-aware tool. The current release line ships a Rust core plus a Python
language plugin; other languages remain future scope.

Part of the [Loom suite](docs/suite/loom.md) of code-archaeology, issue-tracking,
and trust-topology tools.

## Status

**v1.3.0 — current release line.** Scope:

- **Python only.** Other-language plugins (`NG-15`) are v2.0+ scope.
- **Structural extraction + on-demand LLM summarisation.** `clarion analyze`
  walks the corpus and persists entities + edges; `summary(id)` over MCP
  dispatches the LLM lazily, one entity at a time.
- **Local-first.** No mandatory cloud component; the only required network
  egress is the LLM provider during `summary` calls.
- **Stable identity and suite enrichment.** Clarion mints Stable Entity
  Identity (SEI) tokens, serves the federation HTTP read API, emits opted-in
  Filigree scan findings (issue lookups now key by SEI), and enriches MCP reads
  with Filigree/Wardline context without making sibling products mandatory.
- **Wardline trust vocabulary via on-disk descriptor.** The Python plugin reads
  Wardline's NG-25 trust-vocabulary descriptor as a plain file and tags
  trust-decorated entities (`wardline:*`) — without importing Wardline, so a
  co-installed Wardline is not required. Degrades cleanly when the descriptor is
  absent. (Retires the last Clarion-side federation asterisk; see
  [`docs/suite/loom.md`](docs/suite/loom.md) §5.)
- **Guidance authoring.** Operators can author, import, export, and review
  guidance sheets through `clarion guidance`; consult agents consume them
  through MCP and summary cache invalidation.

**Known limitations:**

- **HTTP file language inference uses stored plugin identity plus a narrow
  core-extension fallback.** Plugin manifests declare language and extensions,
  but Clarion does not yet persist a manifest language registry for the
  `/api/v1/files` read path.
- **Some guidance lifecycle surfaces remain deferred.** The in-browser
  staleness-review UI is still tracked separately; authored guidance is
  available through the CLI and MCP read path today.

## What it does today

`clarion serve` exposes a 39-tool MCP surface that a consult-mode agent calls
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
TAG=v1.3.0
curl -L -o clarion-x86_64-unknown-linux-gnu.tar.gz \
  "https://github.com/tachyon-beep/clarion/releases/download/${TAG}/clarion-x86_64-unknown-linux-gnu.tar.gz"
tar xzf clarion-x86_64-unknown-linux-gnu.tar.gz
install clarion-x86_64-unknown-linux-gnu/clarion ~/.local/bin/
pipx install \
  "https://github.com/tachyon-beep/clarion/releases/download/${TAG}/clarion-plugin-python-1.3.0.tar.gz"

# 2. Initialise a project
cd /path/to/your/python/repo
clarion install --path .

# 3. Walk the corpus and persist the structural graph
clarion analyze

# 4. Serve the graph over MCP for consult-mode agents
clarion serve
```

`clarion install` is the one-step agent setup path: it initialises `.clarion/`,
installs the `clarion-workflow` skill for Claude Code and Codex, writes Claude
Code MCP config, upserts Codex MCP config, and installs the SessionStart hook.
Use component flags such as `--claude-code`, `--codex`, `--skills`,
`--codex-skills`, and `--hooks` for partial installs.

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
├── clarion-federation/ Shared federation HTTP types
├── clarion-scanner/    Pre-ingest secret scanner (ADR-013, WP5)
├── clarion-cli/        The `clarion` binary (install, analyze, serve)
└── clarion-mcp/        MCP server exposing the consult tools
plugins/python/         Python language plugin (pyright-backed)
docs/clarion/1.0/      Design ladder — requirements → system-design → detailed-design
docs/clarion/adr/       Authored architecture decision records
```

For the design ladder start at
[docs/clarion/1.0/README.md](docs/clarion/1.0/README.md). The full ADR index
is at [docs/clarion/adr/README.md](docs/clarion/adr/README.md). The Loom
federation doctrine that anchors every cross-product decision is in
[docs/suite/loom.md](docs/suite/loom.md).

## Storage and operations

Clarion keeps project state in a local `.clarion/` directory.
The local-first storage model, the no-NFS constraint, the no-double-analyze
constraint (fs2 advisory lock), and the backup/restore procedure are
documented in
[docs/clarion/1.0/operations.md](docs/clarion/1.0/operations.md).

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
