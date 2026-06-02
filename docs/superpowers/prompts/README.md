# Execution prompts

Drop-in agent prompts for executing the waves of the Clarion "road to first-class" program.
Each prompt is self-contained: paste the fenced block into an agent running in this repo.

**Program context:** [`../specs/2026-06-02-clarion-first-class-program-design.md`](../specs/2026-06-02-clarion-first-class-program-design.md)
(workstreams, dependency graph, wave sequencing §4).
**Task source:** [`../plans/2026-06-02-clarion-integrated-delivery-plan.md`](../plans/2026-06-02-clarion-integrated-delivery-plan.md).

**Numbered waves (0–2) are the suite critical path and end at core paradise.** Wave 3 (WS9) is
the program map's "Later" row, labelled a wave here because it is the one sequential, gated,
post-paradise workstream. The **parallel band** (WS5/WS5b/WS6/WS7/WS8) is NOT a wave — it runs
alongside Waves 0–2 as capacity allows.

| Wave | Workstreams | Gate | Prompt |
|---|---|---|---|
| **0** | WS2 HTTP linkages + WS3 prior-index retention | none (autonomous) | [`2026-06-02-wave-0-execution.md`](./2026-06-02-wave-0-execution.md) |
| **1** | WS1 SEI authority (+ oracle + cutover backfill) | Wave 0 done **+ SEI lock (D1)** | [`2026-06-02-wave-1-execution.md`](./2026-06-02-wave-1-execution.md) |
| **2** | WS4 dossier participation + incremental-skip | WS1 + WS2 (internal) | [`2026-06-02-wave-2-execution.md`](./2026-06-02-wave-2-execution.md) — _closes core paradise_ |
| **3** | WS9 `legis` governance consumption | Wave 2 done **+ `legis` exists** | [`2026-06-02-wave-3-execution.md`](./2026-06-02-wave-3-execution.md) — _governed paradise; forward-staged_ |
| _parallel band_ | WS5 (designed), WS5b (planned), WS6/WS7/WS8 (undesigned) | none | _execution prompts owed for WS5 + WS5b; WS6/7/8 need specs first_ |

## Order

1. **Wave 0** — autonomous, start anytime. Completing it lets SEI lock.
2. **Wave 1** — gated on Wave 0 + SEI lock; the prompt forces a confirm-or-stop gate check.
3. **Wave 2** — closes the dossier (core paradise); gated only on Clarion's own WS1 + WS2.
4. **Wave 3 (WS9)** — governed paradise; gated on Wave 2 + `legis` *existing* (it does not yet).
   Thin on Clarion's side; forward-staged — its gate check blocks until `legis` ships.
5. **Parallel band** — WS5/WS5b/WS6/WS7/WS8 run alongside as capacity allows; not a numbered wave.

The four wave prompts (0–3) are all written. The parallel band has designs/plans for **WS5**
(`specs/…-ws5-mcp-catalogue-design.md`) and **WS5b** (`plans/…-ws5b-advanced-queries-plan.md`) but
no execution prompts yet; **WS6/WS7/WS8** still need design specs. Next authoring targets:
execution prompts for WS5 + WS5b, then the WS6 (guidance) design spec.
