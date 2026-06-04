# Validation Report — 02-subsystem-catalog.md

**Validator:** analysis-validator
**Date:** 2026-05-22
**Target:** `docs/arch-analysis-2026-05-22-1924/02-subsystem-catalog.md`
**Contract:** `temp/task-catalog-template.md`

## Status: NEEDS_REVISION (warnings)

Two factual errors and one minor naming inconsistency found. No critical issues. All seven required H2 sections present with all 10 contract sub-sections each. All bidirectional dependency edges verified. Eight load-bearing claims spot-checked: six confirmed, two failed.

---

## Contract compliance

| Subsystem | Loc | LOC | Role | Resp. | Key comps | Pub iface | Deps | Internal | Patterns | Concerns | Confidence |
|-----------|-----|-----|------|-------|-----------|-----------|------|----------|----------|----------|------------|
| clarion-core | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | High |
| clarion-storage | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | High |
| clarion-cli | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | High |
| clarion-mcp | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | High |
| clarion-scanner | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | High |
| clarion-plugin-fixture | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | High |
| plugins/python | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | High |

All seven subsystems present; numbered 1–7; structural contract satisfied throughout.

---

## Spot-check results

### 1. clarion-core stderr drain (PASS)

Catalog claim: "detached thread … bounded `VecDeque<u8>` of capacity `STDERR_TAIL_BYTES = 64 KiB`."

Evidence: `crates/clarion-core/src/plugin/host.rs:609-620` shows
`Arc<Mutex<VecDeque<u8>>>::with_capacity(STDERR_TAIL_BYTES)` and
`std::thread::Builder::new().name("clarion-plugin-stderr-drain:{plugin_id}")
.spawn(move || drain_stderr_into_ring(stderr, &stderr_tail_for_thread))`. Confirmed.

### 2. clarion-storage writer capacity/batch (PASS)

Catalog claim: "bounded mpsc with capacity 256 … batch commits every 50 writes."

Evidence: `crates/clarion-storage/src/writer.rs:35` `DEFAULT_BATCH_SIZE = 50`;
`writer.rs:38` `DEFAULT_CHANNEL_CAPACITY = 256`; `writer.rs:813` triggers
commit when `state.writes_in_batch >= state.batch_size`. Confirmed.

### 3. clarion-cli analyze ordering (PASS)

Catalog claim: secret scan runs BEFORE BeginRun and BEFORE plugin spawn.

Evidence: `crates/clarion-cli/src/analyze.rs:242` `pre_ingest(...)`; `:244`
`run_lifecycle::begin_run(...)`; `:277` `'plugins: for plugin in plugins`
(spawn-blocking loop with `run_plugin_blocking` → `PluginHost::spawn`).
Order is `pre_ingest` → `begin_run` → per-plugin `PluginHost::spawn`. Confirmed.

### 4. clarion-mcp tool count — FAIL (catalog) / FAIL (discovery)

Catalog claim (`02-subsystem-catalog.md:327, 382, 394`): registry contains
**19** distinct `ToolDefinition` entries; discovery's claim of 20 is stale.

Discovery claim (`01-discovery-findings.md:17, 198, 508`): **20** tools via
`grep -c 'ToolDefinition {'`.

**Ground truth: 19 tools.** Evidence: `crates/clarion-mcp/src/lib.rs:47` is the
struct definition `pub struct ToolDefinition { ... }`; lines 58, 71, 80, 91,
100, 109, 123, 136, 150, 165, 170, 184, 200, 205, 210, 223, 236, 247, 252
are the 19 in-`vec![]` instances. `grep -c "ToolDefinition {"` returns 20
because it counts the struct declaration too. `grep -n 'name: "'` returns
exactly 19 literal-name lines.

**Catalog is correct on the count (19); catalog's framing that "discovery
doc was wrong" is also correct.** The discovery doc's `Confidence Assessment`
section already flagged this risk at line 552 ("20 occurrences of the literal
token, not verified to be 20 *distinct production tools*"); the catalog
resolved it correctly. **No catalog change required.** This is a discovery
artefact to be corrected when discovery is rewritten or in errata.

Names verified (19): `entity_at`, `project_status`, `analyze_start`,
`analyze_status`, `analyze_cancel`, `find_entity`, `source_for_entity`,
`entity_context`, `call_sites`, `callers_of`, `execution_paths_from`,
`execution_paths_ranked`, `summary`, `summary_preview_cost`, `issues_for`,
`orientation_pack`, `index_diff`, `neighborhood`, `subsystem_members`.

### 5. HTTP read API surface (PASS)

Catalog claim: 4 production routes (`GET /api/v1/files`, `POST /api/v1/files/batch`,
`POST /api/v1/files:resolve`, `GET /api/v1/_capabilities`).

Evidence: `crates/clarion-cli/src/http_read.rs:364-372` —

```
let protected = Router::new()
    .route("/api/v1/files", get(get_file))
    .route("/api/v1/files:resolve", post(post_files_resolve))
    .route("/api/v1/files/batch", post(post_files_batch))
    ...
let unprotected = Router::new().route("/api/v1/_capabilities", get(get_capabilities));
```

All 4 routes confirmed. Two additional `Router::new()` instances at lines
1396, 1451, 1524 are inside `#[cfg(test)]` blocks (`/x`, `/boom`, test-only
batch). Confirmed.

### 6. clarion-plugin-fixture protocol methods (PASS)

Catalog claim: implements only `initialize`/`analyze_file`/`shutdown`
(requests) + `initialized`/`exit` (notifications).

Evidence: `crates/clarion-plugin-fixture/src/main.rs:51` `"initialized" =>`;
`:54` `"exit" =>`; `:68` `"initialize" =>`; `:77` `"analyze_file" =>`;
`:116` `"shutdown" =>`. No other method arms. Confirmed.

`lib.rs` is a 3-line documentation stub as the catalog states.

### 7. Python plugin entrypoint name (PASS)

Catalog claim: `clarion-plugin-python`.

Evidence: `plugins/python/pyproject.toml:32-33`:

```
[project.scripts]
clarion-plugin-python = "clarion_plugin_python.__main__:main"
```

Confirmed.

### 8. clarion-scanner pattern count (PASS)

Catalog claim: "12 named rules + 2 entropy classes."

Evidence: `crates/clarion-scanner/src/patterns.rs:194-269` — `default_pattern_meta()`
returns a `Vec<PatternMeta>` with exactly 12 `PatternMeta {}` entries
(`AwsAccessKey`, `AwsSecretAccessKey`, `GitHubToken`, `GitHubFineGrainedToken`,
`GitHubOAuthToken`, `AnthropicApiKey`, `OpenAiApiKey`, `StripeApiKey`,
`SlackToken`, `JwtToken`, `PrivateKey`, `KeywordDetector`). Entropy classes:
`EntropyTuning::BASE64` and `EntropyTuning::HEX` (`entropy.rs:11-18`,
applied at `patterns.rs:64-65`). `grep -c "PatternMeta {"` returns 13 — one
is the struct declaration at line 10. Confirmed.

---

## Bidirectional dependency spot-checks

| Edge | Forward (X→Y) | Reverse (Y inbound lists X) | Status |
|------|---------------|----------------------------|--------|
| clarion-storage → clarion-core | catalog §2 Outbound (`EdgeConfidence`, `RESERVED_ENTITY_KINDS`, line 139) | catalog §1 Inbound line 54 explicitly lists `clarion-storage/src/{commands,query,writer}.rs` | ✓ |
| clarion-mcp → clarion-storage | catalog §4 Outbound line 346 (`ReaderPool::with_reader`, `WriterCmd`) | catalog §2 Inbound line 138 lists `clarion-mcp (lib.rs, tests/storage_tools.rs)` | ✓ |
| clarion-cli → clarion-scanner | catalog §3 Outbound line 222 (`Scanner, Detection, Baseline, SuppressionResult`) | catalog §5 Inbound lines 433-436 list `clarion-cli/src/secret_scan.rs` + 2 submodules | ✓ |

Additional check (informal):
- clarion-cli → clarion-mcp: §3 Outbound line 222 ↔ §4 Inbound lines 341-343. ✓
- clarion-cli → clarion-core / clarion-storage: §3 line 222 ↔ §1 line 52, §2 line 138. ✓
- clarion-cli → clarion-plugin-fixture (dev-dep): §3 line 222 (transitively via test path), §6 Inbound line 521 lists `clarion-cli/tests/wp2_e2e.rs`. ✓
- plugins/python ← clarion-core/clarion-mcp (subprocess only): §7 Inbound line 595 lists `clarion-core/src/plugin/{discovery,manifest,protocol,host}` and `clarion-mcp/src/lib.rs`. Reverse: §1 Outbound External services line 57 acknowledges plugin subprocesses generically; §4 Outbound External services line 350-352 mentions LLM provider subprocesses but not Python plugin specifically — Python plugin is a runtime peer of fixture, not a Rust-callable, so reverse asymmetry is acceptable. ✓

No missing bidirectional links found.

---

## Findings

### F1 — Wrong constant value for `MAX_FILES_PER_PYRIGHT_SESSION` (WARNING)

**Location:** `02-subsystem-catalog.md:575`

**Claim:** "the per-25-files pyright restart policy (`MAX_FILES_PER_PYRIGHT_SESSION = 49`, used at `server.py:215-219`)."

**Reality:** `MAX_FILES_PER_PYRIGHT_SESSION = 25` (`plugins/python/src/clarion_plugin_python/server.py:49`).

The "49" appears to be a transcription of the **line number** (`server.py:49`)
where the constant is defined; the value is 25. The same paragraph already
states "per-25-files" in the prose. The catalog's own Confidence statement
at line 632 cites the correct value: "`MAX_FILES_PER_PYRIGHT_SESSION` constant
is `25` per `server.py:49`." So this is a self-inconsistency within the
same section, resolvable to 25.

**Fix:** change `MAX_FILES_PER_PYRIGHT_SESSION = 49` to
`MAX_FILES_PER_PYRIGHT_SESSION = 25` at line 575.

### F2 — Subsystem name inconsistency (NIT)

**Location:** `02-subsystem-catalog.md:564`

The 7th section is titled `## 7. Python language plugin (\`clarion-plugin-python\`)`.
The discovery doc and the coordination plan refer to this subsystem as
`plugins/python` (e.g. discovery §6, coordination subsystem list). Other
subsection titles use the crate's library name verbatim (`clarion-core`,
`clarion-storage`, etc.).

This is consistent enough to be unambiguous — every reader knows what
"Python language plugin" means in Clarion context — but if the catalog is
indexed by section title, the name does not match the assigned slug. **Fix
optional**: rename header to `## 7. plugins/python` or
`## 7. plugins/python (clarion-plugin-python)` for indexing parity. No
content change required.

### F3 — Catalog §4 Confidence statement undersells dispatch arm count (NIT)

**Location:** `02-subsystem-catalog.md:356`

Catalog says "a 19-arm `match`" for tool dispatch. The registry has 19 tools,
so 19 arms is consistent. The phrasing matches the corrected count. No
change needed; flagging only because the same paragraph in the discovery
doc said 20. This is correct catalog behaviour — not a finding, just
verifying consistency.

---

## Things checked and confirmed correct (no finding)

- stderr drain thread on host.rs (claim #1).
- writer-actor `DEFAULT_BATCH_SIZE = 50`, `DEFAULT_CHANNEL_CAPACITY = 256` (claim #2).
- secret scan → BeginRun → plugin spawn order in analyze (claim #3).
- 19 MCP tools, not 20 — catalog called this correctly against discovery (claim #4).
- HTTP read API: 4 production routes at `http_read.rs:364-372` (claim #5).
- plugin-fixture: 5 protocol methods, no others (claim #6).
- `clarion-plugin-python` binary name (claim #7).
- 12 named pattern rules + 2 entropy classes (claim #8).
- All sampled bidirectional dependency edges resolve in both directions.
- All seven H2 sections satisfy the 10-subsection contract.
- LOC figures in §1–§7 headers cross-check against discovery §6 within
  ±2% (e.g. catalog §1 11,653 vs discovery 11,669; catalog §3 ~6981 vs
  discovery ~6790 — small skew from comment/blank counting choices, no
  factual disagreement).

---

## Confidence Assessment

**High.** I read all seven H2 sections of the target document end-to-end,
the discovery doc end-to-end, and the catalog template. I spot-checked all
eight load-bearing factual claims listed in the validation prompt against
source files by direct read or grep at file:line precision. I verified
bidirectional dependency edges for 3 randomly-selected edges plus 3
auxiliary edges. I cross-checked the MCP tool count by enumerating the 19
`name: "..."` literals in `clarion-mcp/src/lib.rs` and by re-confirming
that the 20th `ToolDefinition {` token comes from the struct definition at
line 47. I confirmed the catalog's tool-count finding (which initially
read as a self-flagged contradiction) is correct and discovery was wrong.

## Risk Assessment

**Low.** The two warnings (F1 transcription error on a numeric constant, F2
section-title naming nit) are surface defects that do not affect downstream
architecture-analysis work in this pass. The MCP tool-count flag — the only
substantive contradiction between catalog and discovery — has been resolved
in the catalog's favour; the catalog correctly identified the off-by-one in
discovery's grep heuristic. F3 is a non-finding included for traceability.

Downstream phases (dependency analysis, diagram generation, quality
assessment) can proceed against this catalog without blocking on the two
warnings; F1 can be fixed inline during the next catalog edit.

## Information Gaps

- I did not re-read every line of every cited source file; I targeted the
  exact line ranges named in the catalog's claims plus the spot-check
  prompts. Other catalog claims (e.g. ADR references in concerns sections,
  finding subcode strings, edge-kind ontology) were not independently
  verified.
- The catalog's "Patterns observed" and "Concerns" sections are
  interpretive; I validated structural presence, not technical accuracy of
  the interpretations. Technical-accuracy review of e.g. the writer-actor
  concurrency claims or the path-jail TOCTOU concern is out of scope for
  this validator and would need a Rust-domain SME.
- LOC figures were not re-counted with `wc -l`; I trusted the per-section
  numbers and cross-checked only against discovery for gross consistency.

## Caveats

- This validator checks structural and factual correctness against the
  contract and against directly-cited source. It does not assess whether
  the analyst chose the right level of abstraction, whether the
  "Concerns" are exhaustive, or whether the public-interface listings
  capture the right granularity for downstream consumers.
- The catalog's discovery-vs-catalog contradiction on tool count is
  textbook validator territory: I resolved it by reading the source. Other
  silent contradictions between catalog and discovery may exist that were
  not surfaced because the prompt did not name them.
- Bidirectional checks sampled 3+3 edges out of ~16 forward edges across
  the seven subsystems; full bidirectional coverage would require checking
  every Outbound line against the reciprocal Inbound list. No misses
  found in the sample, but the sample is not exhaustive.

---

**Recommendation:** APPROVED for downstream phases after fixing F1 (one-token
change: `49` → `25` at line 575). F2 and F3 are optional polish.
