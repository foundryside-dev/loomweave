# Validation Report — 02-subsystem-catalog.md (drift claims)

**Validator:** independent analysis-validator (fresh eyes)
**Date:** 2026-06-02 · Branch `feat/road-to-first-class`
**Document under test:** `docs/arch-analysis-2026-06-02-1522/02-subsystem-catalog.md`
**Scope:** Adversarial, evidence-based verification of 9 load-bearing drift claims against current source + cited doc lines. All evidence was read directly (no reliance on the catalog's own citations).

**Overall status: NEEDS-REVISION**

The eight headline drift findings are overwhelmingly substantiated — the core thesis (shipped code has drifted from `system-design.md`/`detailed-design.md`/CLAUDE.md/ADR-013) is sound and well-evidenced. However **one claim (ADR-013/GCP) is REFUTED** (the catalog overstates an agreement as drift), and **two claims (CLAUDE.md crate count; storage doc-side numbers) need numeric correction**. None of the corrections overturn the catalog's central conclusion, but they must be fixed before the document is treated as canonical. Net: **NEEDS-REVISION**, not BLOCK — the reality-side facts are accurate throughout; the defects are in how three doc-side claims are framed/counted.

---

## Per-claim verdict table

| # | Claim | Verdict | Evidence (file:line I saw) |
|---|-------|---------|----------------------------|
| 1 | §2 async drift — host fully sync; design describes tokio/async/mpsc/streaming/`file_list` | **VERIFIED** | `host.rs:569` `std::process::Command`; `host.rs:637` `std::thread::Builder`; no `tokio`/`.await`/`mpsc` in host.rs. Design `system-design.md:155` `tokio::process::Child`, `:171` `file_list(include, exclude)`, `:175` streaming notifications, `:185` `tokio::sync::mpsc` 100-msg backpressure. `InitializeParams` (`protocol.rs:318-326`) carries only `protocol_version` + `project_root`; design `:167` lists `clarion_version`. |
| 2 | §2 Python drift — design says tree-sitter+LibCST; code uses CPython `ast` only | **VERIFIED** | `plugins/python/pyproject.toml:18-21` deps = `packaging`, `pyright` only (no tree-sitter/libcst). `extractor.py:58` `import ast`. Design `system-design.md:213` "Tree-sitter… LibCST…", `:218` `alias_of` edges, `:219` `python:unresolved:{}` placeholders, `:220` `TYPE_CHECKING` exclusion — none in code. (Note: `python:unresolved` stubs ARE also documented in `detailed-design.md:167,233`, reinforcing the drift.) |
| 3 | §5 policy drift — Anthropic deprecated stub; 1-variant CachingModel; analyze.rs 0 LLM calls; no cost_report | **VERIFIED (with location/count corrections)** | Deprecated stub: `clarion-mcp/src/config.rs:100` `LlmProviderKind::Anthropic`, `:37-40`/`:418`/`:474` `ConfigError::DeprecatedProvider` — **NOT** in `llm_provider.rs` as the catalog implies. `CachingModel` 1 variant: `llm_provider.rs:48-49` `OpenAiChatCompletions`. analyze.rs: **0** references to `LlmProvider`/provider structs/`.complete(`. `cost_report` tool absent (only test-helper string `summary_preview_cost_reports_*`). **Correction:** 4 `LlmProvider` impls ship (`Recording`, `OpenRouter`, `Codex`, `ClaudeCli` — `llm_provider.rs:167,252,549` + Claude), not "three"; `RecordingProvider` is the test/replay fixture. |
| 4 | §6 phase-7 drift — the four `CLA-FACT-*` structural findings absent | **VERIFIED (scope caveat)** | The four named findings (`CLA-FACT-TIER-SUBSYSTEM-MIXING`, `-SUBSYSTEM-TIER-UNANIMOUS`, `-ENTITY-DELETED`, `-GUIDANCE-ORPHAN`) have **zero matches in `crates/` + `plugins/`** — present only in docs (`requirements.md:160`, `ADR-006`, `system-design.md:237`). **Caveat:** a different CLA-FACT finding DOES ship — `CLA-FACT-CLUSTERING-WEAK-MODULARITY` (`analyze.rs:50`). So "absent workspace-wide" must read "the four Phase-7 findings are specified in docs but unimplemented in code"; CLA-FACT structural findings are *not* universally absent. |
| 5 | §8 tool-count drift — 35 tools ship; design says "8-tool subset" / shortcuts "deferred to v1.1" | **VERIFIED** | Exactly **35** `=> match self.tool_…` dispatch arms in `clarion-mcp/src/lib.rs`; the 35 names match the catalog's enumeration verbatim. Design `system-design.md:773` "v1.0 ships an 8-tool subset", `:791` shortcuts "**Deferred to v1.1**". (The cursor/session model is correctly labelled v1.1 and IS unbuilt — correctly excluded from drift.) |
| 6 | §9 route drift — 16 routes; `GET /api/v1/entities/resolve` absent; contracts.md confirms deferred | **VERIFIED (minor breakdown error)** | `http_read.rs:452-514` = exactly **16** production routes (files×3, call-graph×4, identity×4, `_capabilities`×1, **wardline×4**). No `GET /api/v1/entities/resolve` — only `POST /api/v1/identity/resolve` (`:468`). Design `system-design.md:997,1007,383` documents `GET /api/v1/entities/resolve?scheme=` as shipped. `contracts.md:793-805` confirms it ("conformance oracle… deferred to Flow B B.2"). **Correction:** catalog's group breakdown says `wardline×3`; actual is 4 — headline 16 is exact (3+4+4+1+4). |
| 7 | storage drift — 13 tables+FTS5+view / 6 migrations vs "7 tables/1 migration"; application_id/user_version now implemented | **NEEDS-REVISION (doc-side count)** | Reality VERIFIED: migrations `0001`–`0006` = **13 `CREATE TABLE` + 1 FTS5 + 1 VIEW**; `pragma.rs:32` `CLARION_APPLICATION_ID=0x434C_524E`, `:59` `enforce_application_id`, header notes `user_version` bumped by migration runner. **But:** `detailed-design.md:611-760` documents **6** `CREATE TABLE` + 1 `CREATE VIRTUAL TABLE entity_fts` (`:739`). "7 tables" is defensible only if FTS5 is counted as a table; the 6-regular-table figure is more precise. The **"1 migration"** sub-claim is **unsubstantiated** — I found no "single/one migration" statement near the detailed-design schema; `detailed-design.md:1706` references "the numbered-migration feature." Catalog should cite the source for "1 migration" or drop it. |
| 8 | CLAUDE.md drift — Layout says 5 crates/v1.0.0; reality 6 crates/1.1.0 | **NEEDS-REVISION (crate count wrong)** | Reality VERIFIED: `Cargo.toml:3-11` = **6** members; `:13` `version = "1.1.0"`. CLAUDE.md `:7` says "v1.0.0… at 1.0.0" (version drift holds). **But** CLAUDE.md Layout `:12-16` lists only **4** crates (core, storage, cli, plugin-fixture) — it omits **both** `clarion-mcp` and `clarion-scanner`. The catalog's "5 crates" is wrong; the doc lists 4, so the crate drift is 2 missing crates, not 1. |
| 9 | ADR-013 drift — no named GCP service-account rule; ADR-013 claims one | **REFUTED** | `patterns.rs` named rules = 12 (`AnthropicApiKey, AwsAccessKey, AwsSecretAccessKey, GitHubFineGrainedToken, GitHubOAuthToken, GitHubToken, JwtToken, KeywordDetector, OpenAiApiKey, PrivateKey, SlackToken, StripeApiKey`) + 2 entropy — no `Google`/`Gcp`/`ServiceAccount` variant. **But ADR-013:93 does NOT promise a named rule:** it reads *"Google Cloud service-account JSON fragments **(detected via `\"private_key\"` + RSA header)**"* — i.e. coverage via the generic `PrivateKey` mechanism, which is exactly what ships. Doc and code **agree**. The catalog erected a strawman ("ADR claims a named rule") and labelled the agreement as drift. The underlying *fact* (no dedicated GCP rule) is correct; the *drift conclusion* is unsound. Reframe as doc/code agreement (or at most a 🟢 documentation-clarity nit). |

**Tally:** VERIFIED 5 (claims 1, 2, 4, 5, 6) + VERIFIED-with-correction 1 (claim 3) · NEEDS-REVISION 2 (claims 7, 8) · REFUTED 1 (claim 9).

---

## Confidence Assessment

**High.** Every reality-side assertion was checked against primary source (migration SQL, `host.rs`, `protocol.rs`, `pyproject.toml`, `extractor.py`, `config.rs`, `llm_provider.rs`, `patterns.rs`, `http_read.rs`, `lib.rs`, `pragma.rs`, `Cargo.toml`) and every doc-side assertion against the cited lines in `system-design.md`, `detailed-design.md`, `contracts.md`, `ADR-013`, and `CLAUDE.md`. The 35-tool dispatch count and 16-route count are exact mechanical counts. The one verdict reversal (claim 9) rests on the literal ADR-013:93 parenthetical.

## Risk Assessment

- **If claim 9 ships as "VERIFIED drift":** downstream phases (debt roadmap, doc-fix tickets) would file a spurious "ADR-013 lies about a GCP rule" correction against a doc that is actually correct — wasted work and an unwarranted ADR edit (ADRs are immutable once Accepted; this would trigger needless process).
- **If claim 8's "5 crates" stands:** a doc-fix to CLAUDE.md would under-correct (add 1 crate, still wrong) — the Layout actually omits 2 crates.
- **If claim 7's "1 migration" stands uncited:** an unverifiable assertion enters the canonical catalog.
- Claims 1/2/5/6 are the load-bearing "design has drifted" evidence and are rock-solid; the central thesis is **not** at risk.

## Information Gaps

- I did not execute the federation fixtures or run the MCP server; route/tool counts are static (source) counts, consistent with the catalog's own "fixtures not executed" caveat.
- "1 migration" attribution in claim 7: I could not locate the source sentence in `detailed-design.md`; the catalog should cite it or revise.
- Technical *accuracy* of whether the async→sync change is correct/intentional is out of scope (no reconciling ADR was found, which itself supports the drift claim).

## Caveats

- This is a **structural/evidentiary** validation (do the cited facts hold?), not a judgement on whether the drift *should* be resolved by editing the doc vs. the code — that's an architecture decision for the owner.
- Claim 3 and claim 6 are marked VERIFIED because their **headline** claims (deprecated Anthropic stub exists / 16 routes ship / `entities/resolve` absent) are true; the embedded **secondary** details (provider lives in `llm_provider.rs`; "three providers"; `wardline×3`) are inaccurate and should be corrected as line-item NEEDS-REVISION fixes even though they don't move the verdict.

## Required revisions before APPROVED

1. **Claim 9 (§7 ADR-013):** Reverse from drift → doc/code agreement. ADR-013:93 promises coverage *via `private_key` + RSA header*, not a dedicated rule; the code matches. Downgrade to 🟢 "no enumerated GCP rule (by design, per ADR-013's parenthetical)" or remove.
2. **Claim 8 (§8 / CLAUDE.md):** Correct "Layout says 5 crates" → "lists **4** crates, omitting `clarion-mcp` **and** `clarion-scanner`." Version drift (1.0.0 → 1.1.0) is correct.
3. **Claim 7 (§3 storage):** Cite the source for "1 migration" or drop it; clarify "7 tables" = 6 regular + 1 FTS5 (`detailed-design.md:611-760,739`).
4. **Claim 4 (§4 pipeline):** Add the caveat that `CLA-FACT-CLUSTERING-WEAK-MODULARITY` ships (`analyze.rs:50`); reword "absent workspace-wide" → "the four Phase-7 findings are doc-specified but unimplemented."
5. **Claim 3 (§2 policy):** Fix stub location (`clarion-mcp/src/config.rs:100`, not `llm_provider.rs`); reconcile "three providers" vs 4 trait impls.
6. **Claim 6 (§5 HTTP):** Fix breakdown `wardline×3` → `×4` (headline 16 stays).
