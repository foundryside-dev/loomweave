# Clarion v0.1 Design Review

**Original reviewed document (pre-restructure)**: `2026-04-17-clarion-v0.1-design.md`
**Historical note**: this review evaluates the pre-restructure single-file design. The current canonical docset lives in [../README.md](../README.md), with the resulting implementation-level reference in [../detailed-design.md](../detailed-design.md). This file is retained as supporting context, not as normative guidance.
**Review date**: 2026-04-17
**Review method**: Eight parallel specialist reviews across solution architecture, systems dynamics, Python engineering, Rust engineering, security, integration architecture, reality check (symbol/path/API verification), and architecture-decision rigor.
**Verdict**: **Revise before authoring the implementation plan.** Two factual errors must be corrected; four structural decisions need explicit ADRs; the v0.1 scope has three known defects that will damage the suite's cross-tool identity story if not resolved before first ship.

---

## 1. Factual Corrections (Must Fix Before Plan)

These are not design opinions; they are claims in the document that do not match externally verifiable reality.

### 1.1 "SARIF-lite over filigree's `POST /api/v1/scan-results` intake" is incorrect

The design uses "SARIF-lite" throughout (§Abstract line 25, §Principle 4 line 68, §9) as if Filigree's intake is SARIF-shaped. It is not. Filigree's actual intake at `src/filigree/dashboard_routes/files.py:294` takes a flat custom JSON format keyed on `scan_source` + a `findings` array of `{path, rule_id, message, severity, ...}`. Real SARIF uses `runs[].results[].locations[].physicalLocation` with driver/tool objects and fingerprints. Wardline separately emits genuine SARIF v2.1.0, but that is not what Filigree ingests.

**Consequence**: the suite's "shared finding-exchange protocol" as described does not exist. A Clarion-to-Filigree adapter has to translate between them. The design must either (a) state that Clarion emits Filigree's native intake schema and drop the "SARIF-lite" framing, (b) propose a SARIF-compatible extension to Filigree's intake endpoint as a prerequisite, or (c) position Clarion as emitting SARIF upstream of a suite-internal translator. Pick one and say it.

### 1.2 Anthropic model IDs use a notation not found in any real code in this workspace

The config example in §5 uses hyphen-separated version numbers: `claude-haiku-4-5`, `claude-sonnet-4-6`, `claude-opus-4-7`. Real code in adjacent projects (Wardline fixtures, elspeth, Filigree tests) uses dot notation: `claude-sonnet-4.5`, `claude-opus-4.6`. `claude-opus-4-7` specifically has no evidence anywhere in the workspace.

**Consequence**: either the hyphen form is a future API change that hasn't landed yet, or the config example is wrong. This must be pinned against the actual Anthropic SDK version the implementation will use. Shipping with wrong model IDs means `clarion analyze` fails on every LLM call.

### 1.3 Plugin manifest silently omits Wardline groups 9, 12, 13

§2 declares annotation detection for `wardline_groups: [1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 14, 15, 16, 17]`. Groups 9 (operations, co-located in Wardline's source with group 10), 12 (Determinism), and 13 (Concurrency) exist in Wardline and are absent. If the omission is deliberate (these groups lack static-detectable annotations), the manifest needs a comment. If accidental, it's incomplete coverage that will surface as silent gaps in Wardline-derived guidance.

### 1.4 "LSP-style JSON-RPC" is imprecise

LSP's distinguishing feature at the transport layer is its `Content-Length` header framing. The design's protocol shows JSON-RPC messages without specifying framing. Calling this "LSP-style" invites the reader to assume LSP framing; the design should either adopt it explicitly (recommended — it handles partial reads and stream restarts cleanly) or name the alternative (newline-delimited JSON-RPC). As written, the plugin-protocol stream semantics are ambiguous, which matters for resumability after crash (§2 failure table claims "resumes at next file" — only possible if the core can distinguish a complete `file_analyzed` from a truncated one).

---

## 2. Structural Defects That Damage v0.1's Value Proposition

These are design decisions that, if shipped as currently written, will cause visible failures in the first months of real use. They are ranked by severity; each is corroborated by at least two independent review lenses.

### 2.1 Entity ID path-embedding + deferred `EntityAlias` will rot the suite's cross-tool identity story (CRITICAL)

**Corroboration**: Architecture, Python, Integration, ADR reviews.

§3 specifies entity IDs as `{plugin_id}:{kind}:{file_path}::{qualified_name}` and explicitly defers rename tracking (`EntityAlias`) to v0.2. The design's own stated differentiator is the cross-tool graph — Filigree issues reference Clarion entity IDs, Wardline declarations reference them, guidance sheets match against them. Every file move, class rename, or `__init__.py` re-export resolution change silently detaches every reference.

In a 425k-LOC Python codebase that is the validating first customer, renames and moves are weekly events. Python-side specifically: `from auth import TokenManager` where `TokenManager` is defined in `src/auth/tokens.py` and re-exported from `src/auth/__init__.py` — the naive resolver creates two IDs for the same entity. Hundreds of affected entities in a well-structured package.

**Mitigation options, in order of preference**:
1. Make IDs content-addressed + symbolic (`python:class:TokenManager@hash`) with file path demoted to a property. Preserves stability across moves.
2. Move `EntityAlias` into v0.1 scope and build rename detection from the start.
3. Accept the limitation, document it prominently, and scope v0.1 to codebases that don't rename. Honest, but drops the elspeth validation target.

Option 1 is the cheapest and most defensible. Option 3 should not be chosen silently.

### 2.2 SQLite concurrency model is incoherent under the stated process topology (CRITICAL)

**Corroboration**: Architecture, Rust reviews.

§4 claims WAL mode "supports concurrent reads during writes" and says `clarion analyze` "holds the writer lock for the duration of the batch." The example run in §6 shows a 38-minute batch. These are incompatible:

- WAL grows unboundedly during a long writer; checkpointing cannot complete while readers pinned to the pre-analyze snapshot hold the old page.
- `clarion serve` writes to the summary cache during live consult sessions (§5) — these writes will block or timeout.
- Multiple writer sources are unacknowledged: analyze ingesting entities/edges, serve writing summary-cache on consult misses, serve writing session state.

**Mitigation**: specify a writer-actor model (single tokio task owning the write connection, bounded mpsc inbox), short per-N-files transactions rather than a single batch transaction, and explicit `busy_timeout` + checkpoint strategy. Alternative: analyze writes to a shadow DB and atomic-swaps on completion — simpler, and acceptable for single-user local-first deployments.

### 2.3 Python import resolution is named as a plugin responsibility with no design (CRITICAL)

**Corroboration**: Python review.

§2 says the plugin handles "import and name resolution (Python namespace packages, `sys.path`, conditional imports, `__init__.py` re-exports)" — one line, no mechanism. For the claimed 425k-LOC target this is the single hardest static-analysis problem in Python.

**What the spec must decide before the plan**:
- How `sys.path` is discovered (virtualenv introspection via `python -m site`? user-supplied `python_executable` in plugins.toml?)
- What happens for unresolvable imports (emit finding and drop edge? emit placeholder entity?)
- Canonical name policy for re-exports (definition site wins; re-export is an alias edge, not a new entity)
- Policy for conditional imports (`try/except ImportError`, `if sys.version_info`, `TYPE_CHECKING` blocks — first branch wins? all-branches-union?)

Without these decisions, the entity graph is wrong in ways that cascade. §2.1 above (ID stability) compounds: unresolved imports produce spurious IDs that cannot later be reconciled by `EntityAlias`.

### 2.4 Secrets-to-Anthropic and prompt injection have no defenses (CRITICAL)

**Corroboration**: Security review.

Two related threats with severity 9/9 in the security analysis:

- **Secrets exfiltration**: The design sends file content to Anthropic for summarization (§5, §6) with no pre-ingest secret scanner, no redaction pass, no per-file allow/deny beyond include/exclude globs. Any `.env`, test fixture, or committed API key enters Anthropic's API and persists in the summary cache and `runs/<run_id>/log.jsonl` (which the design keeps "for audit").
- **Prompt injection**: Adversarial docstrings/comments in source become attacker-controlled field values in the structured briefing schema. Schema validation doesn't help — the schema validates shape, not semantic content. An LLM-proposed guidance sheet (auto-promoted via the `propose_guidance` capability) then persists attacker text into every future prompt (prompt-cache poisoning).

**Mitigation**: a pre-ingest secret-scanner pass (`detect-secrets` or equivalent) with findings blocking LLM dispatch on unredacted hits, and explicit prompt-injection handling (untrusted-content delimiters, or an "untrusted" role boundary in the prompt structure). Both belong in v0.1, not deferred — the first real user running `clarion analyze` on a repo with a committed `.env` leaks to Anthropic silently.

### 2.5 Provider abstraction is at the wrong layer (HIGH)

**Corroboration**: Integration, ADR reviews.

§5 claims "provider abstraction in place for additional providers without structural change." The `LlmProvider` trait has three methods. But the prompt-caching strategy — exactly four `cache_control` breakpoints placed at specific segment boundaries (§5 lines 876–896) — is baked into how plugins build prompts and how the summary cache is keyed. This is Anthropic-specific at a level below the trait. A second provider (OpenAI, Bedrock) would either receive the four-segment structure it cannot use, or the plugin-side `build_prompt` protocol would need a different path.

**Honest framing**: "the architecture anticipates multiple providers but the plugin-level prompt protocol assumes Anthropic's caching semantics in v0.1." That's a defensible scoping decision; the current framing oversells portability.

### 2.6 HTTP API: 127.0.0.1-no-auth default vs Wardline's claimed CI-cadence usage (HIGH)

**Corroboration**: Security, Integration reviews.

§9 binds the HTTP read API to loopback with no auth by default. §1 says Wardline "runs at commit cadence, pulling current entity state and declared topology from Clarion's HTTP read API." If Wardline runs in CI (the typical commit-cadence posture), loopback-only doesn't fit. The token-auth path is marked "opt-in" but not designed — no format, no rotation, no scoping.

Additionally, loopback is not a security boundary on modern dev hosts: shared Docker host networks, devcontainers, browser DNS-rebinding against a fixed port, and compromised IDE extensions all reach 127.0.0.1. Treating it as secure is a category error.

**Mitigation**: design token auth in v0.1 even if opt-in remains the default. Specify the token format, where it's stored (OS keychain preferred, `.clarion/token` with `0600` mode as fallback), and how Wardline picks it up in CI.

---

## 3. Hidden ADRs That Must Be Written

These decisions are currently buried in prose or presented as foregone. They are load-bearing and should have explicit ADRs before the implementation plan is authored.

| # | Decision | Current state | Why it matters | Priority |
|---|----------|---------------|----------------|----------|
| ADR-001 | Rust for the core | Stated as fact; no alternatives compared | Irreversible (core rewrite = new product); Go, TypeScript, Python + PyInstaller all plausibly meet stated requirements | P0 |
| ADR-002 | Plugin transport: LSP-style subprocess JSON-RPC | Invoked by analogy to LSP/tree-sitter; Wasm, dylib, embedded-Python not considered | Becomes the third-party plugin contract | P0 |
| ADR-003 | Entity ID scheme + rename handling | Path-embedded, `EntityAlias` deferred | Cross-tool identity depends on it; see §2.1 above | P0 |
| ADR-004 | SARIF-lite vs SARIF vs native | Named; doesn't match Filigree reality | Cross-tool interop; see §1.1 | P0 |
| ADR-005 | `.clarion/` git-committable including SQLite DB | Stated as feature; conflict with LLM-derived secrets in DB unaddressed | Operational; affects every user | P1 |
| ADR-006 | Clustering algorithm (Leiden/Louvain specifically) | Named without comparison | Expensive LLM synthesis (Phase 6 Opus calls) rides on cluster quality | P1 |
| ADR-007 | Summary cache key design and invalidation | Specified but alternatives not considered | Guidance edits invalidate broad cache swathes; cost impact | P1 |
| ADR-008 | Filigree file-registry displacement | In a table, not called out as breaking change | Cross-product coordination; affects existing integrations | P1 |
| ADR-009 | Structured briefings vs free-form prose | Strong opinion, no alternatives | Untested assumption about LLM consumption | P2 |
| ADR-010 | MCP as first-class surface — lock-in cost | Enthusiasm, no risk analysis | Strategic; acknowledges Anthropic ecosystem dependency | P2 |

The P0 ADRs must exist before implementation planning. The P1–P2 ADRs can be authored alongside early implementation.

---

## 4. Systemic Risks

These surface from the systems-thinking review and are not immediately visible from a component-by-component read.

### 4.1 Summary cache has no semantic validity signal

The cache key is `(entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint)` (§5). This correctly invalidates on syntactic change. It does not invalidate when:
- An entity's call-graph neighborhood shifts materially (it becomes a hot path) without its own text changing.
- A model tier name maps to a new concrete model version (the mapping changes silently in provider config).
- The guidance sheet's underlying worldview goes stale while its text is unchanged.

Over 12+ months the system can serve high-confidence-looking briefings whose maturity/risks fields are silently out of date. **Mitigation**: add a TTL backstop on cache rows, or a graph-neighborhood invalidation trigger (edge-count delta threshold).

### 4.2 Guidance stock grows without a structural quality signal

Manual-authored and LLM-proposed guidance sheets have no lifecycle coupling to the code they describe. The `reviewed_at` field is a pull query, not a push signal. Combined with the fact that `critical: true` sheets are the last to be dropped from the token budget, a stale critical sheet is the most likely input to future briefings.

**Mitigation**: tie guidance staleness signals to entity churn rate (`git_churn_count`). High-churn entity + old guidance sheet = push finding.

### 4.3 Finding stock has no drain

Every `clarion analyze` run emits findings. There is no feedback from Filigree's triage state back to Clarion's emission policy. A rule that produces 500 suppressed findings with no promotions in 90 days is noise for this project, but Clarion's next run emits the same rule at the same priority.

**Mitigation**: Filigree triage outcomes flow back as a per-rule priority modifier. Simple: if rule suppression rate > N% over M days, emit `CLA-INFRA-RULE-LOW-VALUE` and suggest a configuration change.

### 4.4 Exploration elimination may induce LLM capability atrophy

The stated goal of Principle 2 is to pre-compute structural answers so agents don't have to spawn explore subagents. The second-order risk is that agents trained (by usage pattern) to trust Clarion never develop exploration strategies for cases where the catalog is wrong or incomplete — e.g., latent race conditions visible in runtime traces but invisible to static analysis.

**Mitigation**: the briefing schema should include a `knowledge_basis` field marking each briefing with the class of evidence it rests on (`static_only`, `runtime_informed`, `human_verified`). LLMs consuming the briefing can use this to decide whether to look further. Single-field schema addition with significant downstream calibration value.

---

## 5. Python Plugin Specifics

From the Python engineering review. These are plugin-side concerns that compound with §2.1 (entity ID) and §2.3 (import resolution) above.

1. **Parser dispatch (tree-sitter vs LibCST) is undefined.** Which parser owns which task? LibCST parse failures — what's the fallback? Use `libcst.native` (Rust backend) if LibCST is in the hot path.
2. **Decorator-as-DSL detection is naive.** Direct name match only works for the simple case. Decorator factories (`@app.route("/health")`), stacked decorators (order matters for Wardline semantics), class decorators, and aliases (`validates = validates_shape`) need explicit policy.
3. **Call graph precision is overstated.** AST-only analysis produces reliable `calls` edges for direct same-scope calls, approximate (name-match) for method calls, and nothing for dynamic dispatch. The manifest's `calls: true` capability should carry a `confidence_basis: "ast_match"` qualifier.
4. **`TYPE_CHECKING` block imports are unmentioned.** Treating them as runtime `imports` edges produces false `CLA-PY-STRUCTURE-001` circular-import findings in any typed codebase.
5. **Plugin packaging and interpreter isolation.** `pip install clarion-plugin-python` into an unknown environment risks dependency conflicts with the analyzed project. Recommend `pipx` for isolation; define `plugins.toml` schema (`executable`, `python_version`).
6. **Serial-or-parallel posture.** "May thread internally" is not a posture. Commit to serial for v0.1 (defensible) or specify the parallelism mechanism (partitioned `analyze_file_batch` RPC, or `multiprocessing.Pool` inside the plugin).

---

## 6. Rust Core Specifics

From the Rust engineering review. These are implementation-stack choices that belong in a "Rust stack" addendum.

- **Async runtime**: tokio is nearly forced by the axum/reqwest/sqlx ecosystem; name it explicitly.
- **SQLite driver**: `rusqlite` + `bundled` feature + `deadpool-sqlite` recommended over `sqlx/sqlite` for this workload (lots of prepared-statement reuse, FTS5, JSON1).
- **TLS**: `rustls` + `webpki-roots` to preserve the single-binary portability claim. Native-TLS kills it on mixed Linux distributions.
- **JSON-RPC framing**: commit to LSP-style `Content-Length` framing for the plugin protocol. Required for resumability after plugin crash.
- **Plugin subprocess supervision**: `tokio::process::Child` with explicit `wait()` to reap zombies, SIGPIPE handling, bounded mpsc for the `file_analyzed` stream, crash-loop circuit breaker.
- **Schema specifics**: `UNIQUE(kind, from_id, to_id)` on edges (currently absent), FTS5 triggers wiring `entities` → `entity_fts` (currently absent), generated columns + indices for hot JSON properties (`priority`, `git_churn_count`).
- **stdout hygiene**: plugin protocol must reserve stdout for JSON-RPC; plugin authors must redirect Python logging to stderr. Document this as a plugin-author requirement.

---

## 7. What the Design Gets Right

Not everything in the spec needs to change. The following are load-bearing strengths worth protecting during revision:

- **Principle 3 (plugin-owns-ontology, core-owns-algorithms)** is the most important structural decision in the document. Any pressure to add language-specific logic to the core for convenience should be resisted.
- **Principle 5 (observe vs enforce) is a strict boundary** that keeps Clarion and Wardline from merging into one bloated tool. Protect it — and see §8 below for one place it already leaks.
- **Structured briefings with a fixed schema** (§3) is the right LLM-consumption shape. Prose summaries would lose the entire composability story.
- **Guidance fingerprint as cache key** invalidates exactly the affected summaries when guidance changes — non-obvious and correct.
- **No-silent-fallback posture** in the failure model (§6): every failure emits a finding. This is rare in tools of this class.
- **Explicit non-goals list** (§10) is defensible and scopes the tool appropriately.
- **Pre-computed exploration-query MCP tools** (§8): `find_entry_points`, `find_http_routes`, `find_data_models` family operationalize Principle 2 in a way agents can actually use.
- **Entity-ID-on-Filigree-issues boundary**: the decision that Clarion owns file/entity identity and Filigree owns workflow/lifecycle is a real architectural commitment, not a compromise — which is exactly why §2.1 (ID stability) matters so much.
- **Blake3 for fingerprints, provenance columns on every summary, RecordingProvider for tests**: all correct implementation-level bets.

---

## 8. Observe-vs-Enforce Boundary Leak

Principle 5 states that Clarion's plugin detects *that* an annotation is present; Wardline determines *whether* the annotated code satisfies its declared semantic. The plugin manifest in §2 hardcodes Wardline's annotation names (`@validates_shape`, `@integral_writer`, etc.) and group numbering. This couples Clarion's release cadence to Wardline's vocabulary: adding a new Wardline annotation requires a Clarion plugin release.

**Mitigation**: Wardline ships an annotation-descriptor file (JSON/YAML) that Clarion plugins consume. Clarion detects "the annotation Wardline cares about is present" without hardcoding which ones those are. Preserves Principle 5 and inverts the vocabulary coupling so the faster-shipping tool (Clarion) isn't gated by the slower one (Wardline).

---

## 9. Recommended Revision Sequence

In order:

1. **Correct factual errors** (§1 above). One pass through the document; small edits.
2. **Resolve the SARIF-lite confusion** — either translate at the Clarion/Filigree boundary and drop the SARIF framing, or propose a SARIF extension to Filigree's intake. This is a decision, not an edit.
3. **Decide the entity ID scheme** — either content-addressed + symbolic, or promote `EntityAlias` into v0.1. This is the single decision most likely to prevent downstream rework.
4. **Author ADR-001 (core language) and ADR-002 (plugin transport)**. Even if the answers don't change, the act of comparing alternatives exposes blind spots. Do this before the implementation plan.
5. **Add a `Security` section** covering secret-scanning pre-ingest, prompt-injection handling, `.clarion/` commit posture, HTTP auth for Wardline, API-key file-mode requirements.
6. **Decide the SQLite concurrency model** (writer-actor vs shadow-DB-swap) and document it in §4.
7. **Fill in the Python plugin gaps**: import resolution mechanism, parser dispatch, `TYPE_CHECKING` handling, parallelism posture.
8. **Write the "Rust stack" addendum** (one page) naming the crate choices so implementation doesn't re-derive them.

Items 1–4 are blockers for the implementation plan. Items 5–8 can be written in parallel with early prototype work.

---

## 10. Confidence & Caveats

**Confidence on structural findings is high**. The reviewed design is detailed enough that most concerns above are verifiable against the text. The reality-check review independently verified external dependencies (Filigree API shape, Wardline annotations, `/home/john/elspeth` size) so the factual corrections in §1 are evidence-backed, not inferred.

**Confidence on cost and performance claims is moderate**. The design's `$15 ± 50%` cost estimate and 38-minute runtime for elspeth are unbenchmarked. Acceptance criteria should not close over these numbers until a prototype run exists.

**Limitations**: no implementation code exists yet; some concerns (Rust async structure, Python parser dispatch) may be resolved at code time in ways the design doesn't prefigure. The "resume-driven design" calls in ADR rigor are inferences from document patterns, not claims about the author.

**Not reviewed**: the underlying product hypothesis (that an LLM-assisted code catalog with cached briefings materially improves LLM agent quality on large codebases) was taken as given. Validating it is the purpose of v0.1 against elspeth, not a precondition for design review.
