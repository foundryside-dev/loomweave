# Execution prompts

Drop-in agent prompts for executing the waves of the Clarion "road to first-class" program.
Each prompt is self-contained: paste the fenced block into an agent running in this repo.

**Program context:** [`../specs/2026-06-02-clarion-first-class-program-design.md`](../specs/2026-06-02-clarion-first-class-program-design.md)
(workstreams, dependency graph, wave sequencing §4).
**Task source:** [`../plans/2026-06-02-clarion-integrated-delivery-plan.md`](../plans/2026-06-02-clarion-integrated-delivery-plan.md).

**Wave numbers are committed dispatch slots — once a wave is dispatched, its number is fixed and
never re-used or renumbered.** Waves 0–3 are spent (Wave 0 executing; Wave 3 executed). The
standalone-first-class workstreams take the next free numbers (4–8) going forward; they are ungated
and run concurrently with the suite work, but each is a committed deliverable, not "as capacity
allows." (WS9 sits at Wave 3 because it was dispatched there; numbers are dispatch order, not
dependency order.)

| Wave | Workstream | Gate | Status / Prompt |
|---|---|---|---|
| **0** | WS2 HTTP linkages + WS3 prior-index | none | **executing** — [`wave-0`](./2026-06-02-wave-0-execution.md) |
| **1** | WS1 SEI authority (+ oracle + cutover) | Wave 0 **+ SEI lock (D1)** | [`wave-1`](./2026-06-02-wave-1-execution.md) |
| **2** | WS4 dossier participation + incremental-skip | WS1 + WS2 (internal) | [`wave-2`](./2026-06-02-wave-2-execution.md) — _core paradise_ |
| **3** | WS9 `legis` governance consumption | Wave 2 **+ `legis` exists** | **executed** — [`wave-3`](./2026-06-02-wave-3-execution.md) — _governed paradise; forward-staged_ |
| **4** | WS5 — MCP catalogue | none (concurrent) | [`wave-4`](./2026-06-02-wave-4-execution.md) — _stateless catalogue_ |
| **5** | WS5b — semantic search + reachability | soft: WS5 | [`wave-5`](./2026-06-02-wave-5-execution.md) — _scheduled, not deferred_ |
| **6** | WS6 — guidance maturity | none (concurrent) | planned (`…-ws6-guidance-maturity-plan.md`) → **prompt owed** |
| **7** | WS7 — multi-language plugin | none (concurrent) | planned (`…-ws7-multi-language-plan.md`) → **prompt owed** |
| **8** | WS8 — operational quality | none (concurrent) | planned (`…-ws8-operational-quality-plan.md`) → **prompt owed** |

## Order

1. **Wave 0** — autonomous, executing now. Completing it lets SEI lock.
2. **Wave 1** — gated on Wave 0 + SEI lock; the prompt forces a confirm-or-stop gate check.
3. **Wave 2** — closes the dossier (core paradise); gated only on Clarion's own WS1 + WS2.
4. **Wave 3 (WS9)** — governed paradise; gated on Wave 2 + `legis` *existing*; forward-staged.
5. **Waves 4–8 (standalone first-class)** — ungated, concurrent with the suite waves, committed
   order: WS5 → WS5b → WS6 → WS7 → WS8. **WS5b is Wave 5** — a committed slot, not "someday."

**All nine workstreams are planned.** **Prompts written:** Waves 0–5. **Planned, execution prompt
owed:** Waves 6–8 (WS6, WS7, WS8). Nothing is floated — every wave has a committed slot.
