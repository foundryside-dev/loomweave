# Leverage Analysis: The Five Open Questions as One System Pattern

## The pattern

The five questions look like five concerns. They are one. Each is a **missing-feedback-loop** symptom: a place where operational reality has no path back to the artifact that would change behavior. They wear "parameter" clothing (Level 12) but live at **Level 6 (information flows)** and **Level 5 (rules)**.

The smoking gun is named in the source itself. `crates/clarion-core/src/plugin/breaker.rs:7` flags that operator-tunable limits land "in WP6"; `crates/clarion-cli/src/analyze.rs:74` carries `#[allow(clippy::too_many_lines)]` — an alarm explicitly disabled. The architecture has placeholders where its feedback loops should be.

## Archetype: Drift to Low Performance

Meadows' "drift to low performance" fits cleanly. Each individual deferral is locally rational ("ship 1.0; tune later"; "don't split host.rs mid-sprint"; "PRAGMAs are post-1.0"). The standard erodes silently because there is **no countervailing signal** pushing the other way. The clippy allow is the standard-lowering act made literal in code.

Concretely, each question is the same shape:

| Q | Surface symptom | Missing loop |
|---|---|---|
| 1 (`MAX_FILES_PER_PYRIGHT_SESSION=25`) | parameter with unknown basis | rationale → constant → retune trigger |
| 4 (`application_id`/`user_version`) | absent schema identity | DB collision → detection → action |
| 5 (11+ hardcoded limits) | every tunable is a recompile | production behavior → tuning surface |
| 2 (four monoliths) | 4,703-line `mcp/lib.rs`, etc. | file growth → back-pressure → split |
| 3 (`clarion-llm` in core) | `lib.rs:1` doc-comment contradicts contents | boundary statement → enforcement |

§6.5 ("no `application_id`/`user_version` is a one-line change with no downside"), §6.8 ("every tunable is a recompile today"), and §8 Q5 ("WP6 is named in code comments — what is its current status?") all describe the same gap from different angles. The Python-plugin catalog quote — "every limit is a recompile" — is the cleanest single-line statement of it.

## The highest-leverage intervention

**Level 5 (rule), instantiated as one ADR: "Operational tuning discipline."** Every operational constant must declare: (a) a stated basis (empirical / safety-margin / contract-derived), (b) an operator override surface (`clarion.yaml` field or env var), (c) a retune trigger (the metric or finding subcode that should prompt revisiting it). Apply the same rule-shape — explicit budget + override + trigger — to file size and crate-boundary budgets.

This single rule closes Q1, Q4, Q5 directly (every limit, including the 25-file restart, gains a recorded basis and a tuning surface; PRAGMA identity becomes a "schema identity" instance of the same rule) and structurally addresses Q2 and Q3 (file-LOC and "what belongs in this crate" become budgeted properties with a trigger to act, rather than aesthetic preferences competing with sprint load).

This is **not** "add a config file." A config file without the rule decays back to hardcoded constants on the next sprint — that is exactly how Clarion got here. The rule is what creates the surface; the surface alone is a parameter intervention (Level 12) and parameter interventions to a drift-to-low-performance loop reset the constant without changing the slope.

## What changes, concretely

1. Author an ADR ("Operational discipline: declared basis + override + trigger for every limit"). Cite §6.8 + §8 Q5 + the `breaker.rs:7` comment as the originating evidence. Promote it to Accepted before any further hardcoded limit lands.
2. Apply the rule retroactively to the 11 limits in §6.8 plus `MAX_FILES_PER_PYRIGHT_SESSION` (Q1) and the writer-actor's 256 / 50 cadence constants. The artifact is a table in `detailed-design.md` keyed by constant name.
3. Add `application_id` + `user_version` (Q4) as the schema-identity instance of the same rule — basis stated, trigger ("DB opened with mismatched id → refuse").
4. Adopt file-LOC and per-crate-doc-comment budgets with CI enforcement: `clippy::too_many_lines` is **not** allowed without an ADR-referenced waiver; `lib.rs:1` doc-comment violations are a `cargo deny`-style check. This closes Q2/Q3 by creating the missing back-pressure loop.

## Feedback loops the architecture currently has vs. lacks

**Has (strong):** the path-escape and crash-loop breakers (rolling-window → kill); the writer-actor invariant check (`parent_contains_mismatch` aborts the run); the cross-language fixture parity test (drift caught in CI). These are exemplary balancing loops at the *runtime* layer.

**Lacks:** any equivalent loop at the *design-time* layer. The runtime has back-pressure; the architecture itself does not. File LOC grows, doc-comments drift from contents, limits accrete — and nothing ticks.

## Confidence Assessment

**High** that the five questions cluster as one missing-feedback-loop pattern; the `breaker.rs:7` WP6 reference and the §6.8 enumeration are explicit. **High** that Level 5 is the correct leverage point. **Medium** on the specific archetype label ("drift to low performance" vs. "shifting the burden" — both fit; I chose drift because the standard-lowering is visible in code, not just behavior).

## Risk Assessment

- **Over-bureaucratization:** an ADR that demands a basis for every constant could ossify into ceremony. Mitigation: the basis statement is one sentence; the trigger is one finding subcode. Anything more is the wrong shape.
- **Premature parameter exposure:** exposing all 11 limits as operator surface creates a support burden. Mitigation: the ADR allows "internal, no override" as a declared state — the discipline is *declaration*, not necessarily *exposure*.
- **CI friction on file-LOC budgets:** an aggressive cap on `lib.rs` blocks unrelated PRs. Mitigation: budgets start at current LOC + 10%; ratchet down per release.

## Information Gaps

- WP6's actual status — code comment is the only public reference seen during the from-scratch analysis (the analysis intentionally did not read design docs).
- Whether `MAX_FILES_PER_PYRIGHT_SESSION=25` has an empirical basis the catalog could not surface.
- Whether the file-LOC growth on `mcp/lib.rs` and `analyze.rs` is accelerating or has plateaued — no time-series.

## Caveats

The analysis intentionally did **not** consult `docs/clarion/**` or ADRs; if an Accepted ADR already addresses operational tuning discipline, the recommendation collapses to "promote and enforce the existing ADR," not "author a new one." The `breaker.rs:7` WP6 comment strongly suggests an unauthored plan, not an authored-but-unimplemented one, but the from-scratch method cannot confirm.

## Sources

- `04-final-report.md` §1 (operational-subtlety framing), §6.1–6.8 (the High/Medium smells), §8 Q1–Q5 (the five questions verbatim).
- `02-subsystem-catalog.md` `clarion-core` Concerns ("every limit is a recompile"); `clarion-storage` Concerns (no `application_id`/`user_version`); `clarion-cli` Concerns (`run_with_options` at 570 lines with `#[allow(clippy::too_many_lines)]`); `clarion-mcp` Concerns (4,703-LOC `lib.rs`).
- Source: `crates/clarion-core/src/plugin/breaker.rs:7` ("WP6"); `crates/clarion-cli/src/analyze.rs:74` (`#[allow(clippy::too_many_lines)]`); `crates/clarion-core/src/lib.rs:1` (crate doc-comment vs. contents).
