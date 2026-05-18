# Validation report — 02-subsystem-catalog.md

## Status
NEEDS_REVISION (warnings)

## Summary
The catalog substantially meets the contract: all six subsystems are present with the required sections (Location, Responsibility, Key components, Inbound/Outbound dependencies, Patterns, Concerns, Confidence); every entry cites file:line and assigns an explicit, reasoned confidence; cross-cutting concerns from discovery §5 are reflected. One **internal factual contradiction** survived the assembly: the `clarion-core` entry states `clarion-plugin-fixture` does not depend on `clarion-core`, while the index table, the fixture entry, and the actual `Cargo.toml` agree it does. The error is a single sentence and does not propagate elsewhere, so it is a documentation defect, not a structural one. Several minor LOC drifts (e.g. `clarion-mcp/src/lib.rs` documented as 2620, actually 2623) should be reconciled in a sweep but do not invalidate analysis.

## Findings

### Critical (block proceed)
- None.

### Warnings (document as tech debt, ok to proceed)

1. **Factual contradiction inside the `clarion-core` entry — fixture dependency.**
   Catalog line 123 (in the `clarion-core` Inbound block) says:
   > *"`clarion-plugin-fixture` does **not** depend on `clarion-core` — the fixture binary speaks the wire protocol directly without sharing types."*
   This is false. `crates/clarion-plugin-fixture/Cargo.toml:18` declares `clarion-core = { path = "../clarion-core", version = "0.1.0-dev" }`, and `crates/clarion-plugin-fixture/src/main.rs` opens with three `use clarion_core::plugin::...` lines pulling in `ContentLengthCeiling`, `Frame`, `read_frame`, `write_frame`, plus seven protocol envelope types. The Subsystem Index row E (catalog line 17) correctly shows outbound `core (types only)`, and the fixture entry (catalog lines 524, 536–540) correctly describes the dependency as "Protocol-by-shared-types" via `clarion-core`. The contradiction is local to the one parenthetical in the `clarion-core` entry. Fix: delete or invert the sentence at catalog line 123 to read "`clarion-plugin-fixture` depends on `clarion-core` for the typed protocol structs only (dev-dependency surface); it does not link against the supervisor / writer / MCP paths." This was the contradiction explicitly flagged by the task brief.

2. **LOC drift between discovery, catalog, and on-disk files.**
   - `clarion-mcp/src/lib.rs`: discovery says 2617, catalog says 2620, file is 2623. Acceptable drift (the file grew during B.8) but the catalog cites specific line ranges (e.g. "lines 296–319 for `tool_entity_at`") that will skew slightly. Recommend the line-range table be re-checked against the post-B.8 file or a commit SHA pinned to the analysis date.
   - `clarion-core/src/plugin/host.rs`: catalog and discovery both 3126, file is 3126. Clean.

3. **Catalog claims `clarion-mcp` issues exactly one raw SQL query bypassing `clarion-storage` (`reference_neighbors`).** Verified true (`crates/clarion-mcp/src/lib.rs:2381` is the sole `conn.prepare(` site). The catalog's "Concerns" line on schema coupling stands without modification.

4. **The "Subsystem index" header row shows production LOC totals but does not call out that "Inbound deps" for the index excludes dev-dependencies.** The `clarion-cli` row reads inbound = `(binary — none)`, but `clarion-cli/Cargo.toml:[dev-dependencies]` lists `clarion-plugin-fixture`. This is a strict reading of "Rust library inbound" and is internally consistent with the rest of the catalog, but a future reader skimming the table may infer that the fixture is never linked by CLI. Recommend a footnote under the index table clarifying scope.

5. **No "Confidence" header convention drift.** The contract calls for an explicit Confidence section; all six entries provide one (variously called "Confidence" or "Confidence Assessment"). No revision required; flagging for consistency.

### Spot-checks performed

| # | Claim | Verification | Result |
|---|---|---|---|
| 1 | `clarion-core/src/plugin/host.rs` is 3126 LOC, production code ~1450 LOC | `wc -l` = 3126; `grep '^#\[cfg(test)\]'` returns line 1451; production = lines 1–1450 | ✓ matches catalog |
| 2 | `clarion-mcp/src/lib.rs` is 2620 LOC | `wc -l` = 2623 | Drift +3 (B.8 follow-up commits); not material |
| 3 | `clarion-storage` has 9 `WriterCmd` variants | grepped `commands.rs`: `BeginRun`, `InsertEntity`, `InsertEdge`, `InsertInferredEdges`, `UpsertSummaryCache`, `TouchSummaryCache`, `ReplaceUnresolvedCallSitesForCaller`, `CommitRun`, `FailRun` | ✓ exactly 9 |
| 4 | Migration is 289 LOC with ADR-031 CHECKs on `edges.confidence`, `findings.{kind, severity, status}`, `runs.status` | `wc -l` = 289; CHECK clauses at lines 90 (`edges.confidence`), 108 (`findings.kind`), 112 (`findings.severity`), 125 (`findings.status`), 153 (`summary_cache.stale_semantic`), 201 (`runs.status`) | ✓ matches; catalog also correctly notes `stale_semantic` (line 236 of catalog) which is the additional one |
| 5 | `clarion-mcp` has exactly one raw SQL query (`reference_neighbors`) | Only `conn.prepare(` site in the crate is `lib.rs:2381`, inside `fn reference_neighbors` at `lib.rs:2366` | ✓ exactly one; matches catalog claim |
| 6 | Python plugin's `wardline_probe.py` imports `wardline.core.registry` by name | `wardline_probe.py:38: importlib.import_module("wardline.core.registry")`; `loom.md:70` names this exact asterisk | ✓ verbatim match |
| 7 | `clarion-plugin-fixture` depends on `clarion-core` (resolving the catalog's internal contradiction) | `Cargo.toml:18` and three `use clarion_core::plugin::*` lines in `main.rs` | ✓ depends on it; **catalog `clarion-core` entry is wrong**; fixture entry and index row are correct |
| 8 | `llm_provider.rs` is 948 LOC; OpenRouter strict-JSON path lives at `response_format_for_purpose` with `"strict": true` for both purposes | `wc -l` = 948; `response_format_for_purpose` at line 297; `"strict": true` at lines 303 and 333 | ✓ matches catalog |
| 9 | `pyright_session.py` is 1251 LOC | `wc -l` = 1251 | ✓ matches |
| 10 | `extractor.py` is 744 LOC; `@overload`-stub skip via `_has_overload_decorator` plus safety-net dedup | `wc -l` = 744; `_has_overload_decorator` at line 567; first-wins dedup via `state.duplicate_entities_dropped` at lines 630, 647 | ✓ matches |

### Cross-document consistency
- **Bidirectionality of dependency claims.** Spot-checked four directional claims; all consistent.
  - `clarion-storage` outbound to `clarion-core` (`EdgeConfidence` only) ↔ `clarion-core` is *not* listed as a consumer of `clarion-storage` anywhere ↔ `Cargo.toml` shows `clarion-storage` deps on `clarion-core` but not vice-versa. ✓
  - `clarion-mcp` outbound to both `clarion-core` and `clarion-storage` ↔ both entries acknowledge inbound from `clarion-mcp`. ✓
  - `clarion-cli` outbound to all three core/storage/mcp ↔ each of those entries lists `clarion-cli` as inbound. ✓
  - Python plugin "subprocess of host" ↔ `clarion-core` entry's `host.rs::spawn` description matches; no Rust-link relationship, only stdio + on-disk discovery. ✓
- **Coverage of cross-cutting concerns from discovery §5.** All seven items (entity-ID format, JSON-RPC L4, ontology version semver, edge confidence tiers, summary-cache 5-tuple, Loom federation doctrine, ADR-031 schema-validation policy) appear in catalog entries:
  - Entity-ID format: in `clarion-core` (entity_id.rs, 610 LOC, ADR-003 parity).
  - JSON-RPC L4: in `clarion-core/protocol.rs`, fixture entry "Protocol-by-shared-types", Python plugin `server.py`.
  - Ontology version: in `clarion-storage` schema discussion and Python plugin's `ONTOLOGY_VERSION` duplication concern.
  - Edge confidence tiers: in `clarion-storage::enforce_edge_contract` and `clarion-mcp::optional_confidence`.
  - 5-tuple cache key: in `clarion-storage::cache` and `clarion-mcp::read_summary_inputs`.
  - Loom doctrine: in Python plugin entry's "Doctrine asterisk still live" concern, and `clarion-mcp::filigree` enrich-only discussion.
  - ADR-031: in `clarion-storage` (CHECK constraints discussion and edge contract).
- **Sprint-2 deltas from discovery §7.** All represented:
  - new `clarion-mcp` crate — full entry C exists.
  - OpenRouter strict-JSON path — `clarion-core/llm_provider.rs` entry discusses it at lines 92–94 and 150 of catalog.
  - ADR-031 CHECK clauses — `clarion-storage` entry, lines 236+.
  - B.8 `@overload`-stub fix — Python plugin entry, lines 579 and 614–615.

### Notes
- The catalog is well-evidenced: ~80+ file:line citations spot-check as accurate.
- "Confidence" sections in five of six entries explicitly state what was read in full vs sampled (e.g., `clarion-mcp` entry: "Read 100% of `config.rs`…sampled five handler bodies in full…"). This level of detail is unusually high for the contract and is a strength.
- The Python plugin entry's "AST re-parse duplication" concern is a genuinely novel observation surfaced by the per-subsystem pass, not echoed in discovery; flagging as a value-add.
- The dead-stub finding in `clarion-mcp` ("Dead stateless `handle_tool_call` stub", catalog line 373) is also a fresh observation; verified at `lib.rs:1701` as a real footgun.
- Suggested edit ordering (low cost): (a) fix the fixture/clarion-core sentence at catalog line 123; (b) update mcp lib.rs LOC to 2623 or pin a commit SHA at the head of the catalog; (c) add a footnote under the Subsystem Index that "Inbound deps" excludes `[dev-dependencies]`.
