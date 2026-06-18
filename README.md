# Loomweave

Loomweave is a code-archaeology tool. It ingests a codebase, extracts entities
(functions, classes, modules, and language-specific Rust items) and their
relationships (`contains`, `calls`, `references`, `imports`, `implements`,
`derives`, `inherits_from`, and `decorates`), persists the structural graph to a
local SQLite store, and serves the result to consult-mode LLM agents over MCP. A
coding agent that would otherwise re-explore the tree on every question reaches
Loomweave first and asks a graph-aware tool. The current stable line ships a
Rust core plus first-party Python and Rust language plugins.

Part of the [Weft suite](docs/suite/weft.md) of code-archaeology, issue-tracking,
and trust-topology tools.

## Status

**v1.2.1 — latest stable release.** A maintenance patch over `v1.2.0` that
keeps the consult-surface additions (`include` dossiers, `app_only` scoping, and
per-query caller honesty) while tightening redaction, release metadata, and
reproducible plugin packaging. Scope:

- **Python and Rust first-party plugins.** The Python plugin extracts modules,
  classes, functions, calls, references, decorators, and inheritance edges. The
  Rust plugin extracts modules, structs, enums, traits, type aliases, consts,
  statics, macros, impls, functions, and the core Rust edge set (`contains`,
  `imports`, `implements`, `calls`, `derives`, `references`).
- **Structural extraction + on-demand LLM summarisation.** `loomweave analyze`
  walks the corpus and persists entities + edges; `entity_summary_get` over MCP
  dispatches the LLM lazily, one entity at a time, after explicit live-provider
  opt-in.
- **Local-first.** No mandatory cloud component; the only required network
  egress is the LLM provider during `entity_summary_get` calls.
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
- **Agent orientation.** `loomweave install` installs the `loomweave-workflow`
  skill for Claude Code and Codex, registers MCP config, and installs a
  fail-soft SessionStart hook. The MCP server also exposes orientation through
  `initialize`, `loomweave://context`, and the workflow prompt.

**Known limitations:**

- **Rust analysis is parse-only.** Macro expansions and external-crate edge
  targets are intentionally absent; closures and nested functions fold into the
  nearest named item. See
  [docs/operator/rust-known-limitations.md](docs/operator/rust-known-limitations.md).
- **Rust `references` has a narrow deferred envelope.** Match/let pattern paths
  and enum-variant discriminant expressions are not emitted yet.
- **HTTP file language inference uses stored plugin identity plus a narrow
  core-extension fallback.** Plugin manifests declare language and extensions,
  but Loomweave does not yet persist a manifest language registry for the
  `/api/v1/files` read path.
- **Some guidance lifecycle surfaces remain deferred.** The in-browser
  staleness-review UI is still tracked separately; authored guidance is
  available through the CLI and MCP read path today.
- **Public registries are not the release source of truth yet.** Tagged release
  assets on GitHub remain canonical until a later ADR introduces public PyPI /
  crates.io publication.

## What it does today

`loomweave serve` exposes a 46-tool MCP surface that a consult-mode agent calls
instead of grep-and-read. Write-gated tools such as `analyze_start`,
`analyze_cancel`, `propose_guidance`, and `promote_guidance` are hidden unless
the server policy enables them — with two deliberate exceptions: the
`llm_config_set` / `semantic_config_set` bootstrap tools bypass the gate so a
read-only session can configure (and thereby persistently enable) write tools
and live LLM spend. The core tool families are:

| Family | Examples |
|---|---|
| Navigation and graph traversal | `entity_at`, `entity_find`, `entity_resolve`, `entity_callers_list`, `entity_execution_path_list`, `entity_neighborhood_get`, `subsystem_member_list`, `entity_call_site_list`, `entity_relation_list` |
| Briefing and source inspection | `entity_summary_get`, `entity_summary_preview_cost_get`, `entity_source_get`, `entity_orientation_pack_get`, `project_status_get` |
| Guidance, findings, and federation context | `entity_guidance_list`, `propose_guidance`, `promote_guidance`, `entity_finding_list`, `project_finding_list`, `entity_wardline_get`, `entity_issue_list` |
| Analyze lifecycle and freshness | `analyze_start`, `analyze_status_get`, `analyze_cancel`, `index_diff_get` |
| Faceted and shortcut queries | `entity_tag_list`, `entity_kind_list`, `entity_wardline_list`, `module_circular_import_list`, `entity_coupling_hotspot_list`, `entity_entry_point_list`, `entity_http_route_list`, `entity_data_model_list`, `entity_test_list`, `entity_deprecation_list`, `entity_todo_list`, `entity_test_caller_list`, `entity_high_churn_list`, `entity_recent_change_list`, `entity_dead_list`, `entity_semantic_search_list` |

## Quick start

```bash
# 1. Install from the current GitHub Release
TAG=v1.2.1
curl -L -o loomweave-x86_64-unknown-linux-gnu.tar.gz \
  "https://github.com/foundryside-dev/loomweave/releases/download/${TAG}/loomweave-x86_64-unknown-linux-gnu.tar.gz"
tar xzf loomweave-x86_64-unknown-linux-gnu.tar.gz
install loomweave-x86_64-unknown-linux-gnu/loomweave ~/.local/bin/
pipx install \
  "https://github.com/foundryside-dev/loomweave/releases/download/${TAG}/loomweave-plugin-python-1.2.1.tar.gz"

# 2. Initialise a project
cd /path/to/your/python/repo
loomweave install --path .

# 3. Walk the corpus and persist the structural graph
loomweave analyze

# 4. Serve the graph over MCP for consult-mode agents
loomweave serve
```

The current stable branch can also be installed from a local checkout:

```bash
# From this repository checkout
cargo install --path crates/loomweave-cli
pipx install ./plugins/python
pipx install ./packaging/rust-plugin-dist
```

The `loomweave` PyPI package depends on both
`loomweave-plugin-python==1.2.1` and `loomweave-plugin-rust==1.2.1`; a single
Python install lands the CLI and both plugin executables in the same environment.

`loomweave install` is the one-step agent setup path: it initialises `.weft/loomweave/`,
installs the `loomweave-workflow` skill for Claude Code and Codex, writes Claude
Code MCP config, upserts Codex MCP config, and installs the SessionStart hook.
Use component flags such as `--claude-code`, `--codex`, `--skills`,
`--codex-skills`, and `--hooks` for partial installs.

`loomweave analyze` works without any LLM credentials and is the fastest way to
verify the install. `entity_summary_get` calls require live LLM opt-in plus
`OPENROUTER_API_KEY` (see
[docs/operator/openrouter.md](docs/operator/openrouter.md)).

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
├── loomweave-mcp/        MCP server exposing the consult tools
└── loomweave-plugin-rust/ Rust language plugin core
plugins/python/         Python language plugin (pyright-backed)
packaging/rust-plugin-dist/
                        Rust plugin Python-wheel distribution shim
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
