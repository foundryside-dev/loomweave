# Task: Holistic Discovery Sweep for Clarion Codebase

## Workspace
`/home/john/clarion/docs/arch-analysis-2026-05-22-1924/`

## Hard Constraint — NO existing documentation
You must NOT read any of:
- `docs/clarion/**` (design docs, ADRs, requirements)
- `docs/suite/**` (Loom doctrine)
- `docs/implementation/**` (sprint plans, WP docs)
- `docs/federation/**`
- `docs/arch-analysis-*/` (other than your task spec)
- The "design" / "architecture" narrative portions of `CLAUDE.md` or `AGENTS.md`
- `README.md` for design content; you may glance at it ONLY for build commands.

You MAY read:
- All source under `crates/` and `plugins/python/`
- `Cargo.toml` (root and per-crate), `clarion.yaml`, `.mcp.json`, `Cargo.lock` (for dep names only)
- Per-crate test files
- Fixture files under `fixtures/`
- `.filigree/` and `.clarion/` (filesystem layout only, no issue content)

Findings must derive from code, manifests, and tests — not from prose.

## Scope
Produce a holistic discovery findings document for the Clarion repo. Goal: name what the system *is* from evidence alone, identify entry points, technology, internal/external dependencies, candidate subsystems, and unknowns to investigate per-subsystem.

## Output
Write the full document to `/home/john/clarion/docs/arch-analysis-2026-05-22-1924/01-discovery-findings.md`.

## Structure (follow exactly)

```markdown
# 01 — Discovery Findings (Clarion)

## 1. One-Paragraph Pitch (Inferred From Code)
[2-4 sentences. What is this system? What does it produce? Who is it for? Derived only from code/manifests/tests.]

## 2. Repository Layout
[Tree-style summary of top-level dirs that matter. Note crate count, plugin count, LOC by language, test corpus size.]

## 3. Technology Stack
- **Languages:** [versions from rust-toolchain.toml / pyproject]
- **Rust dependencies (key):** [list 5-15 most architecturally significant — tokio, rusqlite, serde, axum/hyper, clap, etc., from Cargo.toml]
- **Python dependencies (key):** [from plugins/python/pyproject.toml]
- **External processes / services:** [SQLite? Plugin subprocesses? HTTP server? MCP transport? cite where called]
- **Build/test tooling:** [pre-commit, ruff, mypy, nextest, deny — cite source]

## 4. Entry Points
[List every binary `main()` / library entry, with source path and what it does (one line). Cover `clarion-cli` (sub-commands), `clarion-mcp`, `clarion-plugin-fixture`, Python plugin entry.]

## 5. Public Wire Surfaces
[Enumerate every external interface the code exposes. For each: name, location, transport, sketch of message shape. Examples to look for: HTTP read API in `crates/clarion-cli/src/http_read.rs`; plugin JSON-RPC in `crates/clarion-core/src/plugin/`; MCP tools in `crates/clarion-mcp/`; CLI in `crates/clarion-cli/src/cli.rs`.]

## 6. Candidate Subsystems
For each of the 7 candidates listed below, give:
- **Name + path**
- **LOC**
- **Source files (>=1)** with one-line role
- **Direct crate/plugin dependencies (outbound only at this stage)**

Candidates: clarion-core, clarion-storage, clarion-cli, clarion-mcp, clarion-scanner, clarion-plugin-fixture, plugins/python.

## 7. Cross-Cutting Concerns Observed
[Pick from the code: error handling style, logging/tracing, async runtime, configuration loading, schema/migrations, security boundaries (plugin jail, secret scanning), test harness shape. Cite a representative file:line for each.]

## 8. Test Corpus Shape
- Per-crate `tests/*.rs` file inventory and what they exercise (one line each).
- Python plugin: where are tests, how many?
- End-to-end: any shell scripts under `tests/`?

## 9. Open Questions (For Per-Subsystem Phase)
[5-10 specific questions to answer in the catalog phase. E.g., "How is the plugin subprocess sandboxed?", "What is the writer-actor concurrency model?", "How does the federation API authenticate?"]

## 10. Confidence Statement
[Per major claim above, a confidence tag (High/Med/Low) with the evidence file(s).]
```

## How to Work

1. Read `Cargo.toml` (root) for workspace members & deps.
2. For each crate, read `Cargo.toml` + `src/lib.rs` or `src/main.rs` only.
3. Glob source filenames per crate (do not read every file).
4. Read enough to identify modules + their roles.
5. Cite paths as `crates/<crate>/src/<file>.rs` or `plugins/python/src/<file>.py`.
6. Be specific: "spawns subprocess via `Command::new` at `host.rs:142`" not "uses subprocesses".

## Scope discipline
Do not begin per-subsystem deep analysis. Stop at the holistic level. The next phase has dedicated explorers per subsystem.

## Time budget
About 25-35 minutes of subagent work. Token cap: be terse, don't dump file contents.

## Confidence
Mark each section's confidence per the contract. Where evidence is missing, say so explicitly.
