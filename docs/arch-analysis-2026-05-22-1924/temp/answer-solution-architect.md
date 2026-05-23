# Architect Answers — Five Open Questions from §8

Scope: the five §8 open questions in `04-final-report.md`. Source: catalog, ADRs, `gap-register.md`, the actual code paths, and the filigree open-issues queue. No diplomatic softening.

---

## 1. Why `MAX_FILES_PER_PYRIGHT_SESSION = 25`?

**Recommendation:** Keep the value, label it. The constant is an unjustified-in-code heuristic introduced in commit `68b719c` ("Bound Pyright dogfood analysis", 2026-05-20) with no commit-message or in-file rationale (`plugins/python/src/clarion_plugin_python/server.py:49`). The diff added recycling but cited no measurement. Interpreted in context — "dogfood analysis" implies it surfaced during operator-driven runs against Clarion itself — this is an **empirical conservative bound** chosen to stop observed pyright RSS growth, not a derived figure. The 25-file window is short enough to be safe and long enough to amortise the ~150 ms `pyright_init_ms` measured in `sprint-2/b4-gate-results.md:19`.

**Risk of inaction (no documentation, no measurement):** The next operator who hits pyright OOM on a heavy-import file at file 24 has nothing to tune and no way to know whether 25 is conservative or aggressive. Worse, the constant is divorced from `MAX_PYRIGHT_RESTARTS_PER_RUN` (`pyright_session.py:142`) — server-side recycling at 25 files can keep tripping session-side restart caps, and vice versa, with no centralised policy.

**Risk of acting now:** Approximately zero. A one-line comment and a TODO citing `pyright_files_since_restart` plus the session-side restart cap costs nothing.

**Evidence that would change the call:** A measurement run on the elspeth corpus (~425k LOC) sampling pyright RSS per `analyze_file` call. If the curve plateaus before 25, the recycle is wasted re-init cost (~150 ms × N); if it climbs steeply past 15, 25 is already too lax. Either outcome justifies a measured override and config surface.

---

## 2. Post-1.0 plan for the four monolith files

**Recommendation:** Defer all four splits, with one named trigger per file. "No split until it actively impedes a change" is **defensible at this scale** — ~50K LOC of first-party code, 7 subsystems, no inter-crate cycles, a single maintainer, ~57% test/source ratio. The change-amplification cost is real but bounded, and splits done without a forcing function tend to re-coalesce or fragment along the wrong axis. There is no roadmap for these splits in any ADR, sprint doc, or filigree issue — that absence is itself a defensible posture for 1.0, not a planning gap.

The triggers that should fire a split (per file):

- **`clarion-mcp/src/lib.rs` (4,703 LOC):** trigger is adding a 20th tool, or any concurrent-tool-execution work. The tool registry shape (`lib.rs:56-257`) already wants to be a per-category module set. Until then it reads as one file with clearly named sections.
- **`clarion-core/src/plugin/host.rs` (2,935 LOC):** trigger is adding a fifth enforcement layer or changing the four-stage pipeline ordering (`host.rs:866-975`). A natural pipeline-axis / lifecycle-axis / IO-axis split exists but each axis pulls in `mock.rs` (876 LOC, also flagged) and the cost of getting the seams wrong is high.
- **`clarion-cli/src/analyze.rs` (2,549 LOC; `run_with_options` 570 lines):** trigger is the 14th phase. The current 13-phase linearisation is legible exactly because it is one function with one error scope. Extracting per-phase helpers without first naming the contract between phases (entity buffer ownership, breaker tick sites, partial-results semantics) just hides the linearity behind call indirection. The catalog already flagged the in-memory entity buffer (`02-subsystem-catalog.md` line 276) as the load-bearing latent risk — that is the issue worth fixing, not the line count.
- **`clarion-core/src/llm_provider.rs` (2,467 LOC):** see Q3 — this one has a stronger argument for splitting **out of `clarion-core`**, not within it.

**Risk of inaction:** Change-amplification per touch grows monotonically. Every new MCP tool, every new enforcement layer, every new analyze phase makes the next refactor more expensive. At 6,000 LOC `mcp/lib.rs` will be genuinely hard to navigate.

**Risk of acting now:** Premature splits chosen on the wrong axis, retest cost across all four files concurrently, and architectural drift while the splits are in-flight. Splitting `host.rs` mid-sprint while WP6 wires the config surface (see Q5) would be especially expensive.

**Evidence that would change the call:** A concrete change request that touches 3+ of these files in the same PR, or a contributor onboarding that stalls on "where do I put new tool X". Either signal flips the calculus.

---

## 3. Will `clarion-llm` become a crate?

**Recommendation:** Yes, and it is **already named in `docs/clarion/1.0/detailed-design.md:1745`** as one of the intended workspaces (`clarion-core`, `clarion-cli`, `clarion-plugin-protocol`, `clarion-api`, `clarion-llm`). The current placement of `llm_provider.rs` in `clarion-core` is an **expedient, not a deliberate boundary call**: the detailed design says where it goes and the code does not yet match. `clarion-core/lib.rs:1` advertises the crate as owning "domain types, identifiers, and provider traits" — the OpenRouter `reqwest` transport and the two CLI-subprocess providers (Claude, Codex shellouts) are neither.

**Risk of inaction:** `clarion-core` pins `reqwest` (with rustls) and CLI-subprocess machinery into the same crate that supervises plugins. That widens the trust surface of the host runtime — a malicious dependency in the LLM HTTP stack lives inside the crate that handles `pre_exec` `setrlimit` for plugin children. The blast radius argument alone is sufficient. Secondary: every new LLM provider drives a recompile of `clarion-core`, which forces recompile of every downstream crate.

**Risk of acting now:** A pre-1.0 crate split during release cut adds churn. ADR-030 narrowed WP6 to a single MCP `summary(id)` tool — the LLM surface is at its minimum scope right now, which is paradoxically the **best** time to split (small surface = small move) but also the time when "ship the tag" pressure resists any refactor.

**Evidence that would change the call:** None substantive. The recommendation here matches the documented intent. The only open question is timing — pre-`v1.1.0` or after.

---

## 4. `application_id` / `user_version` PRAGMAs

**Recommendation:** Add both, now. This is already filed as **`clarion-f2a984fd6d` — `[v1.0 blocker] Set PRAGMA application_id on writer open`** (P1, ready, blocks two other issues), with a fix specified in `docs/implementation/v1.0-tag-cut/gap-register.md` STO-02: `PRAGMA application_id = 0x434C524E` ("CLRN") and assert on open. There is nothing to decide here. The architect already decided; the issue is open and ready.

`user_version` should also be set, even though the application-level `schema_migrations` table (`schema.rs:17-91`) tracks migration state. The two solve different problems: `application_id` identifies the file as Clarion's; `user_version` provides a fast PRAGMA-level read of "what migration level is this" without opening the table. The current model fails confusingly when a non-Clarion sqlite file happens to live at `.clarion/clarion.db` — `apply_migrations` will create tables in someone else's database. `application_id` mismatch turns that into a hard fail at open.

**Risk of inaction:** Bounded but real. Today's only victim is the operator who points `clarion install --path` at a directory whose `.clarion/clarion.db` is from a sibling tool, or a future v2.0 Clarion that opens a v1.0 file. Both are tractable until installed DBs exist in the wild.

**Risk of acting now:** Effectively zero. PRAGMA additions don't break readers; the `apply_write_pragmas` site already exists.

**Evidence that would change the call:** None.

---

## 5. Operational tuning roadmap (WP6 / 11+ hardcoded limits / 25-file pyright / 256/50 batch cadence)

**Recommendation:** Ship 1.0 with the limits hardcoded. Land the config surface in WP6 post-1.0 as **one ADR-021-aligned change** rather than dripping per-constant overrides.

ADR-021 §4 already names the config keys for four of the eleven limits — `plugin_limits.max_frame_bytes` (floor 1 MiB), `plugin_limits.max_records_per_run` (floor 10,000), `plugin_limits.max_rss_mib` (floor 512 MiB), and named `expected_max_rss_mb` in the manifest. These are **promised** by an Accepted ADR and **not implemented**. `breaker.rs:7`'s comment names WP6 as the home for this surface. The `2026-04-19-wp2-tasks-4-to-9-handoff.md:203` line says the crash-loop parameters are "hard-coded for Sprint 1; config surface deferred to WP6". WP6's v0.1 scope was narrowed by ADR-030 to the on-demand `summary(id)` MCP tool — **the operator-tunables work was not folded into the narrowed WP6 scope and is currently un-homed**.

The catalog's eleven-limit list (`02-subsystem-catalog.md` line 97) plus the 25-file pyright restart plus the writer's 256-cap mpsc and 50-write batch cadence (`writer.rs:35, 38, 813`) plus the per-batch HTTP cap of 256 queries (pinned on the wire by ADR-034, so **not** operator-tunable) all want different homes:

- **Plugin host enforcement (frame ceiling, entity cap, RSS, NOFILE, NPROC, field bytes, stderr tail, header bytes, callee-expr bytes):** belong in `clarion.yaml:plugin_limits.*` per ADR-021. Eight of the eleven.
- **Writer-actor cadence (256 mpsc, 50-write batch):** belong in `clarion.yaml:storage.*` per the same shape ADR-011 already hints at.
- **Pyright session recycle (25):** belongs in `plugin.toml` (plugin-owned policy) or per-language plugin config, not core-side.
- **Crash-loop window (>3 / 60 s):** belongs in `clarion.yaml:plugin_limits.crash_loop` per `handoff:203`.
- **Batch HTTP cap (256):** pinned on the wire, not tunable (ADR-034 §3).

**Risk of inaction:** ADR-021's "configurable" claim becomes false advertising on the operator's first read. The elspeth-scale test (425k LOC Python) is the named first customer; if RSS or entity cap defaults are wrong, the operator has no remediation short of patching source and rebuilding. Every limit is a recompile today; the catalog is correct that "operator can tune" is aspirational.

**Risk of acting now (pre-1.0):** Scope creep on a release cut. The shape of the config block is contestable and locking the wrong shape into `clarion.yaml` before the first real elspeth run is worse than landing nothing. WP6 is a post-1.0 ADR-named home; **that is the correct place**.

**Evidence that would change the call:** Either (a) elspeth-scale dogfood data showing any single hardcoded limit is wrong by an order of magnitude — that constant goes to WP6 priority; or (b) a v1.1 issue triaged from the field where the operator could not work around the limit without a custom build. Currently neither exists in the filigree queue.

---

## Confidence, risk, gaps, caveats

**Confidence:** High for Q1 (commit message confirms post-hoc bound, no design-doc rationale), Q3 (`detailed-design.md:1745` names the crate), Q4 (already a v1.0-blocker issue with a documented fix). Medium-High for Q2 (no roadmap exists; defensibility argument is judgement, not evidence). Medium for Q5 (ADR-021/ADR-030/handoff fragments triangulate the WP6 home but no single doc spells out the full eleven-constant migration plan).

**Risk assessment:** The only standing High-risk item in the §8 set is the LLM transport living in `clarion-core` (Q3), and only because of the blast-radius coupling to the plugin supervisor. Q4 is High-severity but Low-risk because the fix is filed and ready. Q1, Q2, Q5 are Medium and Medium-Low respectively.

**Information gaps:** (a) No measurement curve for pyright RSS vs. files-processed exists in the repo; Q1's "conservative bound" interpretation is inferred from the commit title and the 150 ms init cost in `b4-gate-results.md`, not directly attested. (b) No post-1.0 sprint plan exists for the four monolith splits; Q2's "defer with named trigger" recommendation is the architect's call, not a documented decision. (c) The WP6 config-surface scope after ADR-030's narrowing is not written down anywhere — Q5 reconstructs it from fragments.

**Caveats:** All five questions ask for stances the source code cannot reveal. Each recommendation here is contingent on the design-doc intent the user wrote down (ADR-021, ADR-030, detailed-design.md §workspaces). If those intents have shifted in conversation since the docs were last updated, the recommendations shift accordingly. The strongest signal in this set is Q4 — the gap-register and the filed P1 issue make that one not a judgement call.
