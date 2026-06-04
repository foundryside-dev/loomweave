# Per-Subsystem Catalog Entry Contract (Clarion)

Read `/home/john/clarion/docs/arch-analysis-2026-05-22-1924/01-discovery-findings.md` first (4 minutes) to ground in shared vocabulary. Then deep-read your assigned subsystem.

## HARD CONSTRAINT — No design docs

Do NOT read `docs/clarion/**`, `docs/suite/**`, `docs/implementation/**`, `docs/federation/**`, prior `docs/arch-analysis-*/`, ADRs, sprint READMEs, or the design/architecture-narrative portions of `CLAUDE.md` / `AGENTS.md` / `CHANGELOG.md`. Findings must derive from source, manifests, tests, fixtures, migrations, and configuration.

## Allowed reading

- All source files in your assigned subsystem (every file you need).
- Cross-subsystem `pub` interfaces (read minimally to confirm dependency edges).
- `Cargo.toml` (per-crate and workspace), `plugin.toml`, `pyproject.toml`.
- Migration SQL, tests, fixtures.

## Output contract

Append a single H2 section to `/home/john/clarion/docs/arch-analysis-2026-05-22-1924/02-subsystem-catalog.md`. Use a file lock pattern: read existing content first, then write the full file back with your section appended. If empty, create with an H1 header `# 02 — Subsystem Catalog (Clarion)` first.

Your H2 section must follow this exact structure:

```markdown
## [N. Subsystem Name]   ← N is the position you were assigned; subsystem name matches discovery

**Location:** `path/to/crate-or-package/`
**LOC:** [source LOC / test LOC]
**Crate type / role:** [binary, library, plugin manifest type]

### Responsibility
[One paragraph (3-5 sentences). What does this subsystem own? What concern does it abstract from the rest of the system? Cite the public surface that proves it.]

### Key components
[3-7 bullets. For each: `path/to/file.rs:line-range` — role. Be specific about line ranges of the load-bearing types/functions, not just file paths.]

### Public interface (outbound)
[The types/functions/traits/binaries this subsystem exposes to other parts of the system. For Rust: `pub` items in `lib.rs` or the binary's CLI surface. For the Python plugin: the JSON-RPC methods it implements. Bullet each one with one-line description and source location.]

### Dependencies
- **Inbound (who calls this):** [crates/files that import this subsystem]
- **Outbound (what this calls):** [other Clarion subsystems + external crates that matter]
- **External services:** [SQLite, subprocess plugins, HTTP, etc., with calling site]

### Internal architecture
[2-4 paragraphs describing how this subsystem is organised internally. Concurrency model? Module split? State ownership? Error model? Cite specific files/lines for each claim.]

### Patterns observed
- [3-6 bullets naming concrete patterns: actor + bounded channels, command pattern, capability gating, parser+lexer split, etc. — with file:line evidence]

### Concerns / Smells / Risks
[Frank assessment. Include things like: file size, coupling smell, missing tests, performance suspect, error-handling gaps. Cite evidence. If nothing observed, say "None observed in this pass" and explain why.]

### Confidence: [High|Med|Low]
[One sentence with evidence. e.g., "High — read all 11 source files end-to-end and all 4 test files; cross-checked with two callers from clarion-cli."]
```

## Process

1. Read the discovery doc (~3 min skim).
2. List your subsystem's source files (`find <path> -name '*.rs'` or `*.py`).
3. Read `lib.rs` (or `__init__.py`) + the top 3-5 modules by LOC end-to-end.
4. Skim test files to identify behavioral contracts.
5. Identify inbound callers via `rg "use clarion_<name>" crates/` (Rust) or import scan (Python).
6. Write your H2 section using the structure above.

## File-locking pattern (to avoid clobbering parallel siblings)

Because multiple agents may write to `02-subsystem-catalog.md` in parallel:
1. Read the current file.
2. Build a single Write call that contains: existing content (verbatim, including the H1 header) + your H2 section appended.
3. Write the whole thing.
4. If a sibling beat you and the file content changed between your read and write, re-read and retry with your section appended again.

If you do not want to risk clobbering, instead write your section to `temp/catalog-<subsystem>.md` and the coordinator will assemble them.

**Preferred:** write to `temp/catalog-<your-subsystem-slug>.md` (e.g., `temp/catalog-clarion-core.md`). The coordinator will merge.

## Be terse
Aim for ~300-450 lines of catalog text. No file dumps. Quote at most 5-10 lines of code total across the whole section, and only where decisive.

## Confidence statement is mandatory
Mark High/Med/Low. Justify with what you read.
